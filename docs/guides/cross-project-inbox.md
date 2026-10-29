# Cross-project inbox guide

Send messages between agents in different projects without losing context across iterations.

Every message is one envelope on the bus log, and the chats store records it. No per-message file is written.

## Onboarding a project to the inbox fleet

Onboarding is a single step: set the project name so peers can address you.

**Set the project name.** Edit `<repo>/.fno/config.toml` and add (or confirm) `project: <name>` at the top. The name MUST match what peers will use in `fno agents mail send --to-project <name>` (example known names: `footnote`, `acme-backend`, `acme-web`, `acme-docs`, `acme-blog`, `marketing`). If the field is missing, the send command errors with `set 'project:' in .fno/config.toml or pass --from-name`.

Once named, peers can `fno agents mail send --to-project <you>` and you read with `fno agents mail unread`.

## Send a message

```bash
fno agents mail send --to-project acme-web --kind heads-up \
    --body "New region data source live in PR 112"
```

The kinds are `send` (the default), `heads-up`, `question` and `fyi`. A `question` also escalates to a human.

Optional flags: `--reply-to <msg-id>`, `--ref-pr <N>`, `--ref-node ab-...`, `--ref-gate <name>`.

## Read your inbox

```bash
fno agents mail unread             # messages addressed to you past your cursor
fno agents mail unread --json      # machine-readable
fno agents mail show <msg-id>      # one message
```

At each turn boundary, `fno agents mail drain-self` delivers this session's own mail.

## Acknowledge a message

```bash
fno agents mail ack msg-a4f1b2
```

## Reply to a message

```bash
fno agents mail reply --to msg-a4f1b2 "silent-failure-hunter HIGH on swallow_errors_in_dispatch.py"
```

The reply joins the original message's thread and goes back to its sender. If the chats store does not hold the msg-id, the reply is a self-note that names it (`sent orphan reply ...`).
