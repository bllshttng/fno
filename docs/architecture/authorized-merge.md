# One authorized merge operation

`crates/fno-agents/src/authorized_merge.rs` answers one question for every path that lands a PR. Is this exact head authorized to merge or to arm, and what happens next.

## Why it exists

Two callers used to answer that on their own. They answered it differently.

`fno do pr merge` ran a long guard chain. The plan hold, the in-flight review hold, the posture fold, the automerge floor, the coverage gate, the checks verdict, the base lineage. Then it merged.

`fno-agents finalize` armed GitHub's native auto-merge queue at a green terminal. Its chain was shorter and different. It never read the in-flight review hold, so a queue armed at the terminal shipped the code a review was still fixing. When it cannot read the covered head, it also dropped `--match-head-commit`. A racing push then landed an unreviewed head through the queue.

A guard on one of two reachable merge paths is decorative. The two chains are one chain now.

## The shape

A caller builds a `Request` and reads one `Outcome` back.

```rust
let outcome = authorized_merge::run(
    &authorized_merge::RealProbes,
    &Request { cwd, pr, effect: Effect::Arm, approved, auto_merge_source, .. },
);
```

`Effect::Merge` merges now. `Effect::Arm` hands the PR to GitHub's queue. The decision is identical. Only the effect differs.

The receipt keeps them apart. `Armed` is a queue entry, a promise GitHub keeps later. `Merged` is a merge that landed. A caller that reads one as the other stops watching a PR that never merged.

| Outcome | Means | Caller does |
|---|---|---|
| `Merged` | the merge landed. `note` says how. `cleanup_failure` names a post-merge step that failed | post-merge follow-ups |
| `Armed` | the queue owns it now | nothing; the queue merges on green |
| `Authorized` | a `decide_only` pass cleared; nothing ran | its own pre-effect step, then ask again |
| `Held` | retryable: a hold, a pending check, an already-armed PR | retry later |
| `Refused` | needs an operator: no grant, below the floor, a stale base, an unbound PR | escalate |
| `HeadChanged` | the head moved between validation and the effect | re-evaluate |
| `Unknown` | an instrument could not answer | retry; never read it as clear |
| `Failed` | the effect ran and failed | report |

By default the gate merges a green, mergeable PR as it is. Only a GitHub conflict pulls main into the branch. `auto_merge.require_fresh_ci` (default false) turns on the freshness gate this section and the next describe. With it on, the gate merges a PR only after its CI tested the current main. The same key arms the `base_overlap` hold, which holds a behind PR while parallel lanes run and the base move touches its files. The test is ancestry, not a timestamp. The gate compares the PR against its base (`compare/{base}...{head}`). A `behind_by` above zero holds as `ci_base_stale` only on file overlap: the newer base commits touch a file the PR changes. A behind head with no shared file merges (user ruling, 2026-10-07). A behind head whose merge has no conflicts also merges (user ruling, 2026-10-18). An unreadable file set holds as retry-later and takes no slot. The run-timestamp heuristic this replaced answered the wrong question. A run can start after the base tip moves yet still run at a head cut before it. An unreadable freshness read holds with `retry later`, never a merge. The gate covers `Effect::Arm` too. Main has no branch protection, so GitHub merges an armed green PR at once.

After a main merge, every open PR with older CI is stale. A stale PR can prove its merge result clean: the merge tree into the current base computes clean. The repo-wide static step also passes on it. That PR merges on the current settle, with no fold and no slot. Main CI and the main-red hold catch a combined break. Folding on every stale settle restarted a full CI run each time. On a base that moves faster than CI, the holder chased the fold forever, and green PRs starved behind the slot. A stale PR whose merge result cannot verify instead walks the fold. While the slot is free, `decide` holds it with `ci_base_stale` and gives it the slot. The holder then has GitHub merge main into its branch (`PUT pulls/<n>/update-branch`, pinned to the head it read). That is a merge commit, never a rebase, so CI reruns against the current main. The slot claim then records `retested` metadata, so the retest runs once per slot hold. The mark lands in whichever store the slot lives in: the rootless primary, or the legacy repo-root store a migration window can still hold. If the base moves again, that settle still merges on the real `gh pr merge` call. The review coverage carries over as `carried_base_sync`. A conflict or a failed call holds with the manual remedy: merge `origin/main` into the branch and push. The receipt says `merge_slot_held`. While a PR holds the slot, all others are `Held`, even with fresh CI. This keeps another merge from invalidating the holder's retest. When the holder merges or closes, turns red, takes a dispatch hold, or its 60-minute lease expires, the slot releases. A held holder can satisfy none of the other three, so a hold releases the slot. An admitted holder that arms keeps the slot until its merge lands. The read-only preview reads the same two exits. A proven merge, or a self-held slot with its retest spent, reads no stale blocker. pr status never holds a merge the gate will take. The 60-minute TTL covers one retest, the measured 31-minute rust-ci maximum, one 10-minute sweep tick, and margin. The lease uses `<canonical repo>/.fno/claims`, keyed `merge-slot:<base_ref>`.

## The decision, in order

1. **One guarded fetch.** `fno do pr info` gives the number, the head, the state, the body, and whether GitHub's queue owns the PR. The armed flag rides that same payload. A second `gh pr view` probe describes a different head.
2. **Terminal state.** A merged or closed PR holds before every other guard. The guards below protect a merge that has not happened yet. An "unreviewed" answer about a landed merge sends a caller hunting a defect that blocks nothing.
3. **Outside PR.** The gate refuses a head repo that is not the base repo, and a non-owner-class author. Outside is final: no law row, flag or config admits an outside PR. The refusal names the rebuild path, never an admit command. An origin read that cannot answer is `Unknown`, never a quiet clear. The section below carries the rule.
4. **Authority.** A per-run refusal (`auto_merge_approved: false`) outranks every grant, except the head-scoped operator grant below. An explicit per-run env grant (`auto_merge_source: env-target-auto-merge`) satisfies the standing arm on its own. Otherwise the LIVE config decides. A manifest snapshot never outlives an operator who flips the switch off mid-flight. Then the automerge posture floor.
5. **Node binding.** The graph must see the PR. Three keys decide, in order. A node id the branch names. A node whose own back-pointer carries the PR. A closure line in the body (`Fixes <id> [<id>...]`. The retired `Backlog-Closure:` spelling still reads). The same predicate answers the lead board's untracked warning, so the board and the gate cannot disagree. When all three miss, the merge refuses. The remedy is to bind it: pick or file the node, write the closure trailer onto the body, retry. A revert or a hotfix binds the same way. There is no bypass flag. The team merges, and not only the operator. A bypass flag is one the agents pass to themselves. The gate stays silent where the repo keeps no backlog. A graph with no node under the canonical root has nothing to bind to. A graph or a body that cannot be read is `Unknown`, never a verdict.
6. **The dispatch hold** (`fno do pr hold-check`), fail-closed.
7. **The per-PR hold**, an operator law row at `pr-hold:<owner/repo>#<n>`, fail-closed beside the dispatch hold. Held, not refused: the owner releasing it clears it. The section below carries the exact commands.
8. **The merge freeze**, fail-closed beside the per-PR hold. An off-pin PR holds until the user lifts the pin. The section below carries the record and the lift.
9. **The in-flight review hold** (`fno do pr review-hold check`), fail-closed. Coverage answers what verdicts EXIST for a head. It cannot say that a review runs right now with its findings uncommitted.
10. **The pin.** The covered head comes from the caller's own coverage gate, or from the `review_coverage` journal. An unreadable head is `Unknown`. There is no unpinned fallback. A head that no longer matches the PR's is `HeadChanged`.
11. **Base lineage** (`fno do pr base-lineage-check`), fail-open. A refusal on a gh hiccup makes auto-merge silently never work. That reads exactly like nobody opting in.
12. **Merge result** (`fno do pr merge-result-check`), fail-open. The merge tree is computed locally and the repo-wide ruff + mypy step runs on it. The same merge tree also runs the repo's preamble budget from its own copy. Two PRs that each pass their own base can no longer merge into a main over the byte ceiling. When git joins hunks that never met on one machine, two green parents merge red. Held, not refused: the remedy is rebase, fix, push, retry.
13. **Checks**. The caller asks for these or leaves them out. `--auto` IS the wait for the checks, so the arm path leaves them out and the queue enforces them server-side.

## The head-scoped operator merge grant

The per-run no-merge refusal used to name only out-of-band escapes: merge by hand, or re-dispatch. That door is closed. The one sanctioned remedy is a law row at a subject that names the exact head:

```
merge-grant:<owner/repo>#<pr>@<40-hex head>
```

An operator records it in their own terminal, attended:

```
fno backlog decide 'merge-grant:owner/repo#2131@abc...' 'merge authorized for this head' --authority operator
```

The reader lives in `merge_grant.rs` (`head_grant_status`) and shares the waiver's trust shape. Only rows whose `authority_source` is `operator` count. A harness session cannot mint one, because `fno backlog decide` refuses `--authority operator` from any agent session. The decision must equal `merge authorized for this head` exactly: row existence carries no polarity, identical duplicates read granted once, and disagreeing rows read conflicting. A `chat_attested` row at the subject is invisible to the reader.

Head invalidation is structural. A push changes the head, the subject no longer matches, and the new subject has no rows: the refusal stands until the operator grants again. No manifest is rewritten.

The grant supersedes only the per-run layer. Live config, the posture floor, the holds, the pin, and every later guard still run. A merge that passes them carries `"merge_grant": "operator head grant <short head>"` in its receipt.

## Outside pull requests and the owner's per-PR hold

The predicate lives in `pr_admission.rs` (`outside_reason`). When the head repo is not the base repo, the PR reads outside. A deleted fork reads outside. When the `author_association` is not `OWNER`, `MEMBER` or `COLLABORATOR`, the PR reads outside. Bots read outside like any other non-member author.

No outside PR is merged, healed, bound or reviewed by fleet lanes, and no law row, flag or config admits one. The refusal names the rebuild path, never an admit command. Worth-pursuing work enters through its linked issue as a backlog node. Normal triage decides. The fleet builds accepted work on its own branch and credits the contributor with a `Co-authored-by: <login> <numeric-id>+<login>@users.noreply.github.com>` trailer. The PR steward below is the contributor's way in.

The decision reads the two gates in the order above: `outside_pr` refused (and `Unknown` on an unreadable origin read), `pr_hold` held. The preview walk carries the same two as blockers, so `fno do pr status` never says ready where merge refuses. `Effect::Merge` and `Effect::Arm` both pass through the decision, so finalize's auto-merge arm and the daemon's grant queue obey them with no change of their own.

The per-PR hold is an operator law row at `pr-hold:<owner/repo>#<n>` with decision `do not merge this pull request`. It is not head-scoped, so it survives pushes, and it applies to every PR, the owner's own included. Hold:

```
fno backlog decide 'pr-hold:<owner/repo>#<n>' 'do not merge this pull request' --authority operator
```

Release by superseding it:

```
fno backlog decide 'pr-hold:<owner/repo>#<n>' 'release' --authority operator --supersedes <decision id>
```

Recording a hold disarms an armed auto-merge queue at once: the decide door runs `gh pr merge <n> --repo <owner/repo> --disable-auto` after the ruling lands. If that call fails, the ruling still stands (the record exits 0) and stderr names the manual command.

The origin facts never change for a PR: head repo, base repo, author association, login. The outside probe caches them per PR at `cache/pr-origin` inside the state root. One GitHub call per PR, ever. No TTL. Deleting the file forces a fresh read. That is also the remedy for an author whose association changed after the first read.

Heal skips every outside PR with a `skip_outside` receipt, fail closed on an unreadable origin read. Heal pushes to origin, the base repo, so it can never fix a fork's branch anyway. `guarded_push` refuses the protected branches (`main`, `master`, `develop`, `dev`) even under a force flag.

Both open-PR binding legs and the board's mergeable queue skip a row whose `isCrossRepository` reads true. Reconcile never binds a fork PR. Pr-watch, the grant queue, the review lanes and the board never see one. Only a positive true skips. A row without the field classifies as before.

Uncovered cases, kept on purpose: a hand bind of a fork PR with `fno backlog update <id> pr_number=<n>` still cannot merge, but nothing gates a review-fix dispatch onto it. The owner's raw `gh pr merge` is the owner's call. A same-repo branch from a non-member bot is caught by the merge and heal predicate but not by the binding legs, which read cross-repo only. A member who lost access keeps reading inside until the cache file is removed. A useful bot PR can never merge through fno. The owner merges it by hand.

### The PR steward

Heal's apply pass ends with one steward walk over the same open-PR pages it already read. The walk is `run_pass` in `pr_steward.rs`, called from `heal.rs` before the tick event. The gate is an operator law row under the `outside-contributions` subject with a `global` scope, read through `fno backlog decisions outside-contributions --lane law --state live --json`. Any read or parse failure turns the steward off (`steward_off_no_law`), never on. A GitHub budget backoff also turns it off.

Per outside, non-bot PR the steward takes one action per pass, tracked in stage files at `cache/pr-steward/<owner>/<repo>--<pr>.json` inside the state root. With no stage: it reads the PR comments once. When no earlier steward marker is there, it posts the issue-first comment asking the contributor to open an issue and link it with `Closes #<n>`. It then reads the PR body for the issue link: a URL, an `owner/repo#<n>` shorthand, or a bare `#<n>` that is not a self-reference. It files the issue as an intake node with `fno backlog idea --source-kind from_observation`. The origin evidence is the issue URL, and the details carry a `Co-authored-by` credit trailer. Later passes hand the pull request diff, the linked issue, the contributor identity and a capped backlog-find view to a tool-less reader model. The view arrives as `UNTRUSTED` nonce-fenced blocks. The reader returns one verdict: `pursue`, `decline`, or `needs_clarity` with a sanitized question. The note lands on the intake node and, for `needs_clarity`, the question is posted to the PR. Every comment carries an HTML marker, so a rebuilt or wiped stage file costs at most one comments read and never a duplicate comment.

## The merge freeze

A merge freeze holds a release window. The team sets it, and only the user lifts it. The code is `merge_freeze.rs`. With `store.remote_url` set, the record lives in the primary's `merge_freeze` table, so a freeze set on one machine holds a merge made from any other. Without it, the record lives in `merge-freeze.json` in the agents home, beside the fleet breaker. If the primary holds no record, a machine with a primary still reads its local file, so a freeze an older build wrote keeps holding. An unreachable primary holds the merge. The lift is a ruling, so it reaches another machine only through the shared backlog (`store.share_backlog`).

The record names a subject, a `set_by`, a reason, a repo and a pin. The pin is a candidate sha, a list of PR numbers, or both. A set with no pin refuses, because a total stop belongs to the fleet breaker. The team sets it through the `authorized-merge` command:

```
fno-agents authorized-merge '{"op": "freeze-set", "subject": "rc 0.4", "set_by": "lead", "repo": "owner/repo", "sha": "<40 hex>", "prs": [3170]}'
```

The pin key is `sha:<first 12 hex>` and `prs:<n>+<n>` in ascending order, joined by a comma. A PR on the pin's PR list merges. A PR of another repo merges. Every other PR of the frozen repo holds. Version 1 records (an `allow` list with no repo) still read, as a PR pin over every repo.

Four doors read the freeze. `fno do pr merge`, the pr-watch merge arm and the finalize auto-merge arm all reach `authorized_merge::run`, which reads the freeze as the `merge_freeze` probe beside the per-PR hold. The decision answers `held`, and `fno do pr status` lists a `merge_freeze` blocker. The fourth door is GitHub's own queue. A freeze set disarms every open armed PR of the repo that is off the pin, and the receipt lists them under `disarmed`. A failed disarm never fails the set. The receipt names the PR and the `gh pr merge <n> --repo <owner/repo> --disable-auto` command. The probe also disarms an armed off-pin PR at its next gate read. The pr-watch pre-check sends no repo, so it reads every PR as inside the frozen repo. That is an early skip, never a leak.

Only a live `operator` ruling at the lift subject lifts the freeze:

```
fno inbox decide 'merge-freeze:<owner/repo>@<pin key>' 'lift' --authority operator
```

The user records that row at a terminal (an `operator` row), or says lift in chat and a session records their words. The chat row is a `chat_attested` law row whose rationale starts `user in chat:` or that stores a `quote` (`decision_index::is_user_ruling`). A bare `chat_attested` row does not lift it. Any process a harness session started can mint one, so only the user's words show the user ruled. The merge grant keeps the operator-only shape. A team or agent row at the subject does not lift it either. A row at another pin's subject does not lift it. A retracted lift row freezes the window again. An unreadable record or decision store holds the merge.

The `freeze-clear` op only tidies the record. It removes the primary row and the local file. Its `evidence` must be the decision id of a live `operator` ruling at the current lift subject. Free text refuses, and the refusal names the user's terminal door.

A new pin re-arms the freeze. While the current pin is unlifted, a set refuses. After the user lifts it, a set with the same pin refuses, and a set with a new pin writes the record. The new pin has its own lift subject and no ruling there, so it holds again.

A `release-pin` on the release record moves the sha of a live freeze on the same repo to the new pin. Only the sha changes: the sha admits no PR, so the move never widens the freeze. The new sha gives a new lift subject, so a lifted freeze re-arms. The record is in [distribution](../distribution.md#the-release-record).

The pr-nudge ladder pauses on a frozen PR, so its owner gets no merge retry while the freeze holds.

The freeze does not cover a merge through the GitHub web page or a bare `gh pr merge` outside fno. The fleet breaker's total stop (`fno agents incident stop --hold merges`) stays separate.

## Red main and the revert lane

While main's latest settled run is red, the gate refuses every merge to main. One lane is exempt: a PR whose node carries the `main-repair` tag. That lane is how main turns green again.

The `main_revert` daemon arm opens that lane on its own. It is off by default. Turn it on per repo:

```toml
[main_repair]
auto_revert = true
revert_after_minutes = 10   # default 10
```

Each minute the arm reads main's verdict through the gate's own cached reading. If a red head stays red past the window, is a `Merge pull request #N` commit, and has a green first parent, it is the culprit. The hold stops further merges once main reads red, so the red head is usually that merge. The arm then files a node tagged `main-repair`. It opens a PR that reverts the merge (`git revert -m 1`) and names that node. It asks the author of PR N to re-land the change with the fix. The revert needs one CI run. A fix forward can wait behind a flaky test and a CI queue: on 2026-10-26 main stayed red about 3 hours that way.

The arm refuses to guess. If the parent run is red, cancelled or missing, or the head is not a PR merge, it journals `ambiguous` and opens nothing. A red whose failing checks all sit on heal's flake ledger journals `flaky`. Every outcome is one `main_revert_check` row per red head, so a daemon restart never opens a second revert. The arm never merges the revert. The owner merges it through this gate.

## The effect

`gh pr merge <n> [--auto] --<strategy> --match-head-commit <head>`.

The pin is never optional. It is the last guard between the decision and a racing push. If the head moved, gh refuses the whole call.

No `--delete-branch`, on either path. Its LOCAL delete fails from inside the worktree that holds the branch. That made the best-disciplined merge the one most reliably reported failed. Remote cleanup is a separate step.

A non-zero gh exit is never taken at face value. The PR's own state is re-read. When a post-merge step fails after the server-side merge landed, gh also exits non-zero. A branch another worktree holds recovers through the REST endpoint, which carries the same `sha` pin.

The recovery and the cleanup failure ride separate fields. A recovery that worked is how the merge landed, not trouble around it. Folded together, every worktree-held merge reported partial.

## Who calls it

- **`fno do pr merge`** (`cli/src/fno/pr/_merge.py`) asks twice. The `decide_only` pass comes first, so a merge about to be refused never publishes its coverage status receipt. The second pass re-runs the whole chain and closes the window the first one opened.
- **`fno do pr verify --kind merged`** (`crates/fno-agents/src/pr_verify.rs`) asks once, then re-reads the PR before it reports. This command's name is its contract, so the receipt is not its last word.
- **`fno-agents finalize`** arms at a green terminal. What stays in finalize is what only the terminal knows: the merge-gating opt-out claim, and the optional-App review evidence.

Python reaches it through `fno.rust_binary.command_call("authorized-merge", payload)`, the single door. An unreachable binary answers `unknown`, never a clear merge. A merge whose authorization nobody read is not an authorized merge.

That includes a binary too OLD to know the command. A deployed `fno-agents` from before this landed answers `unknown`, so `fno do pr merge` holds at exit 2 rather than merging unauthorized. The remedy is `fno doctor update --rust`, and `fno doctor` reports the lag. Hold, never break, was the point of routing the door through one named refusal.

## After a queue-armed merge

When the queue lands a merge later, no fno process is in the loop. The remote ref used to stay behind forever, because the merge command's cleanup step never runs on that path.

The PR watcher tick already detects the confirmed merge. It pays the step there, through the merge command's own `_post_merge_remote_delete`. One cleanup implementation. No second watcher, no second store. Only the MERGED arm reaches it, so a pending or unreadable state never deletes anything. It is warn-only: cleanup never fails the merge it follows.

Local branches and worktrees keep their own lifecycle. See [worktree-mechanics](worktree-mechanics.md).

## Merge provenance

Every merge hop writes one `decision_span` row to the project journal beside the repo. The row carries `trace.actor_session` and `trace.actor_kind`, so "who merged PR N" has a durable answer that survives the session.

The merge owner (`authorized_merge::run`, via `crates/fno-agents/src/merge_provenance.rs`) writes `merge_landed` or `merge_armed`. The `path` attr names the lane: `pr_merge`, `pr_watch`, or `finalize`. The gh proxy's draft door writes `merge_requested` with `path: gh_proxy` for every delegated gh merge argv. A PostToolUse Bash hook (`hooks/merge-capture.sh`, through the `graph-get` stdin door) writes `merge_requested` with `path: hook`. It is the backstop for gh that skipped the proxy.

When reconcile closes a node whose PR merged with no fno record anywhere, it writes `merged_outside_fno` with `merged_at` and `merge_sha`. No record means the user in the GitHub UI or another machine. The Rust close also stamps `closed_by: {session, actor_kind}` on the node row, and the daemon close adds `path: reconcile`.

GitHub alone cannot tell a fleet merge from a user merge while both share one token. The query that answers the node's own record is `jq 'select(.type=="decision_span" and .data.pr==N)' <repo>/.fno/events.jsonl`. A GitHub App identity for fleet merges is the standing question that will make GitHub itself show the difference.
