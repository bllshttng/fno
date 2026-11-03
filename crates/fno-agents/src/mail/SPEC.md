# The mail bus: wire contract

This page states what a writer, a reader and a delivery lane must do. It is written from the code in this directory. Each section names the file that holds the rule. The conformance suite is `crates/fno-agents/tests/mail_conformance.rs`. A change to a rule here changes the suite in the same commit.

Scope: the local bus log and the delivery of one message to a running session. The cross-host relay is a front-door path outside this contract (see Cross-host relay).

## Envelope

File: `envelope.rs`.

One bus row is one JSON object on one line. The current version is 1.

| Field | Type | Rule |
|---|---|---|
| `v` | integer | Always written. The writer sets `ENVELOPE_VERSION`. |
| `id` | string | `fmail-` and 12 hex digits. Unique on the log. |
| `ts` | string | UTC, `YYYY-MM-DDTHH:MM:SSZ`. |
| `thread` | string | The thread root. See Threads. |
| `from` | string | The sender handle. |
| `to` | string | The recipient handle or scope. |
| `kind` | string | The message kind. Kinds are not a closed set. |
| `from_harness`, `to_harness`, `in_reply_to`, `from_session`, `from_model`, `to_kind`, `origin`, `subject` | string | Written only when non-empty. |
| `word_count` | integer | Written only when known. |
| `meta` | object | Written only when non-empty. |
| `body` | string | Always written, last. |

Field order is byte order. The writer emits the fields in the order of this table. A reader must not depend on order.

A row with no `v` reads as version 1. A row with a `v` above the reader's version is not readable. The reader skips it and prints a warning that names the segment, the line and both versions. A `v` that is not a whole number reads as 1.

## Threads

File: `envelope.rs` (`thread_root`).

The `thread` field holds the thread root. A first message is its own root: `thread` equals `id`. A reply carries its parent's `thread`. A reply to a reply therefore carries the same root as the reply it answers. There is one level. The reply names its direct parent in `in_reply_to`.

## Addresses

File: `address.rs`.

| Form | Meaning |
|---|---|
| `name` | A handle or session id on this machine. |
| `name@host` | A handle on another machine. Exactly one `@`, both sides non-empty. |
| `--to-lead <scope>` | Whoever holds the lead role for the scope. |
| `--to-project <project>` | The one live peer that answers for the project. |

A host compares by its normal form: lower case, trimmed, with one trailing `.local` removed. `iMac2D2.local` and `imac2d2` name the same host. A target with two `@` is refused with the text of `target_error`.

Parsing is pure. Resolving a handle to a live session is the caller's job.

## Delivery states

File: `state.rs`.

A live send ends in one of six states.

| State | Meaning |
|---|---|
| `queued` | On the bus. No live lane reached the recipient. |
| `injected` | Bytes written or RPC sent. No turn record holds the envelope yet. |
| `parked` | Typed into the composer, or taken into the harness queue. |
| `accepted-new-turn` | The harness's own turn record holds the envelope id, as a new turn. |
| `accepted-steer` | The same record, folded into the turn that was running. |
| `failed` | A reason string says why. |

If the harness's own turn record holds the envelope id, the lane reports an accepted state. Without that record, a lane never reports one. A started turn is not that proof.

The bus keeps state history as append-only rows with `kind: state`. `meta.of` names the message. `newest_state` folds them in log order. An accepted state is terminal: a later row never lowers it. Among the other states the newest wins. With no state row, a `landed` row reads as accepted. A legacy `delivery` field maps as follows: `typed` is `injected`. `hosted` and `cross-session` are `accepted-new-turn`. Anything else is `queued`.

## The log

File: `log.rs`.

The live log is `messages.jsonl`. Rotated segments are `messages.jsonl.<N>`. A higher N is older. A reader walks the segments from the highest N down, then the live file. Readers take no lock.

The log path resolves in this order. The first rule that yields a value wins.

1. `FNO_BUS_DIR`, then `messages.jsonl` in it.
2. The `paths.bus_dir` config value, with a leading `~` expanded.
3. `db/mail/bus/messages.jsonl` under the state root. A root the daemon has not folded yet answers `bus/messages.jsonl` until its next start.

A writer appends one line under an exclusive lock on `messages.jsonl.lock`. The lock waits up to `FNO_BUS_LOCK_TIMEOUT_SECS` (default 5). If the live file is at or over `FNO_BUS_MAX_BYTES` (default 5 MiB), the writer rotates first. It drops segment `FNO_BUS_RETAIN` (default 5). It shifts the others up by one. It moves the live file to segment 1. A rotation failure never loses the message. The file mode is 0600.

A reader skips a blank line silently. It skips a line that is not JSON, and a row that is not readable at its version, with a warning in the form `bus log: skipping ... <segment>:<line>`. An unreadable segment warns as a whole.

## Lanes

File: `lane.rs`.

A lane is one way to put bytes in front of a running session. A send runs the hold gate first. A hold stops every lane and the send fails with the bus-only reason. After the gate, the first lane in this list whose rule matches the target carries the send. No later lane runs. A lane keeps its own fallbacks inside itself.

| Order | Lane | Serves a target when |
|---|---|---|
| 1 | `Pane` | The row has a mux pane. A dead codex pane heals to the codex thread. |
| 2 | `CodexThread` | The row is a codex app-server thread. |
| 3 | `Keeper` | The row is a keeper-backed thread and has a session id. |
| 4 | `DaemonDeliver` | The harness is not claude and not opencode. This covers ACP and the footnote-harness thread. |
| 5 | `ClaudeSwitchboard` | Always. Claude and opencode: switchboard, then the control socket. |

The order is part of the contract. A lane returns a delivery state. It does not write the bus row. The caller stages the body row and writes the durable fallback.

## Cross-host relay

A target of the form `name@host` is relayed over ssh by the front door before any lane runs. It is not a lane. The cross-host table has its own columns and carries no envelope version.

## What this module does not do

It does not mint ids, read clocks, resolve a handle, guard a sender, write the chats store or write a receipt. Those need the fleet, and the caller does them. `scripts/ci/check-mail-module-deps.sh` fails on any file here that names a path outside `crate::mail::`.
