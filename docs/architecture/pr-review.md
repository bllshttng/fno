# PR review in the mux

The review view shows a node's PR diff and lets you comment on its lines. One send writes every comment as one blocking finding on the node, so the worker cannot promise done until it answers them.

Open it with `d` on a backlog node's detail, or `r` on a seat page in the org view. Those two show it full screen.

From a chat pane, `prefix+I` opens it as a right split for the focused pane's session. The chat stays live on the left. The split reads the session's node, then that node's PR from the store. A session with no node, or a node with no PR, shows `no PR yet`. `prefix+I` again, `I` inside the split, or Esc closes it. A click on the chat pane hands the keyboard back with the split still open. The split's left edge drags, and `-` and `+` resize it, from 20 to 80 percent of the terminal. The width holds for the session and is shared with the file viewer panel.

The keys are in [mux-keys](../mux-keys.md#inside-the-pr-review).

## What it reads

- The diff: `gh api repos/<o>/<r>/pulls/<n> -H "Accept: application/vnd.github.diff"`. This returns the same bytes as `gh pr diff` and spends no GraphQL request.
- The head: `gh api repos/<o>/<r>/pulls/<n> --jq .head.sha`.
- The recipient: `fno-agents claim status node:<id>`. A review opened from a seat names that seat and its machine instead.

Both `gh` reads run off the UI loop, once per open, with a 20 s bound. A failed read shows `diff not read: <first error line>`. A diff over 1 MiB or 20,000 rows shows its first part and says so.

## What a send writes

`S` runs `fno-agents backlog note --graph <g> <node> --blocking --stdin --block-cmd "gh api ... (diff)"`. The body is one head line, then each comment numbered:

```text
review of PR #7 (o/r) at abcdef01: 2 comments. Fix each, then resolve this finding.

1. src/a.rs:12
> let x = 1;
rename this

2. src/a.rs:9 (removed)
> let y = 2;
keep this
```

A comment on a removed line anchors to its old-side number. A body over 5,000 chars (the finding prose limit) refuses with the count and keeps the drafts. It is never truncated.

## How the worker answers

The note engine sends the claim holder a pointer through the wrapped mail door. The worker reads the review with `fno backlog notes findings <node>`, fixes each line, and clears it with `fno backlog note --resolve <fid>`.

The receipt says what the door did: `pointer sent to <holder>`, `holder <holder> not reached here: <reason>`, or `pointer to <holder> still sending after 6s`. The finding is written first, so the gate holds whichever line you read.

## Why a finding and not mail

Mail cannot carry a review. The mux mail door caps text at 400 chars on one line. The mail style gate caps a body at 80 words. A mail also does not hold the worker. The stop gate refuses a promise while a finding is open.

## What it does not do

It does not post to GitHub, because that writes on a public repo under your account. It does not edit files, show the worker's replies, or review uncommitted work. The diff pane (`prefix+D`) covers uncommitted work.
