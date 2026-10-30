//! The `mail` front commands on the Rust leg: the do-not-disturb hold, its
//! release timer, the pane-send renderer, the session seed-provenance leaf,
//! the `team` broadcast, the outbound sent view, the withdraw leaf, the
//! unread listing, the ack leaf, the raw prompt-line send, the reply leaf,
//! and the drain-self leaf. Dispatch only; the commands live in [hold],
//! [pane_prepare], [seed_provenance], [team], [sent], [withdraw], [unread],
//! [ack], [send_raw], [reply], and [drain_self].
pub mod ack;
pub mod drain_self;
pub mod hold;
pub mod pane_prepare;
pub mod reply;
pub mod seed_provenance;
pub mod send;
pub mod send_raw;
pub mod sent;
pub mod team;
pub mod unread;
pub mod withdraw;
pub mod you_pairs;

pub fn run(rest: &[String]) -> i32 {
    let first = rest.first().map(String::as_str);
    if rest.is_empty() || first == Some("--help") || first == Some("-h") {
        print_help();
        return if rest.is_empty() { 2 } else { 0 };
    }
    let help_asked = rest[1..]
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--help" || a == "-h");
    match rest[0].as_str() {
        "send" if help_asked => {
            print!("{SEND_HELP}");
            0
        }
        "reply" if help_asked => {
            print!("{REPLY_HELP}");
            0
        }
        "hold" | "dnd" => hold::run_hold(&rest[1..]),
        "hold-release" => hold::run_hold_release(&rest[1..]),
        "seed-provenance" => seed_provenance::run(&rest[1..]),
        "team" => team::run_team(&rest[1..]),
        "withdraw" => withdraw::run(&rest[1..]),
        "sent" => sent::run_sent(&rest[1..]),
        "send" => send::run(&rest[1..]),
        "reply" => reply::run(&rest[1..]),
        "pane-prepare" => pane_prepare::run(&rest[1..]),
        "unread" => unread::run(&rest[1..]),
        "ack" => ack::run_ack(&rest[1..]),
        "drain-self" => drain_self::run_drain_self(&rest[1..]),
        other => {
            eprintln!(
                "fno-agents mail: unknown command {other:?} (hold | dnd | hold-release | seed-provenance | team | withdraw | sent | send | reply | pane-prepare | unread | ack | drain-self)"
            );

            2
        }
    }
}

const SEND_HELP: &str = "\
Usage: fno agents mail send [OPTIONS] [name] [message]

  Send a message to a registered agent or a project.

Arguments:
  name     Agent name, short-id, or full session id. A codex head-8 is a
           65.536-second UUIDv7 bucket, not random, and is refused against a
           codex row; claude ids are UUIDv4, so either form works.
  message  Message to send (async, fire-and-forget).

Options:
  -H, --harness <str>      claude | codex | gemini (optional; used for
                           mismatch check).
  -c, --cwd <str>          Working directory context.
  --from-name <str>        XML-safe sender. Unset: session handle or 'fno' for
                           agents; project for inbox notes.
  --from-self              Stamp the sender with this session's own canonical
                           mail handle (the reply handle `fno whoami` shows).
                           Fails loud (exit 2) with no ambient harness
                           identity.
  --to-project <str>       Anycast: deliver to whoever works on this project
                           (live if exactly one peer, durable queue if none).
                           Use instead of <name>.
  --to-lead <str>          Anycast over the role: deliver to whoever holds
                           this scope RIGHT NOW, resolved at send time; no
                           live holder queues nothing.
  --any                    With --to-project, break a multi-live-peer tie
                           (most recent activity wins).
  -k, --kind <str>         Inbox kind (heads-up | question | fyi); pair with
                           --to-project. Omit for a plain agent-to-agent send.
  --reply-to <str>         With --kind: msg-id being replied to (appends to
                           the existing thread).
  --persist <str>          With --kind fyi: 'memory' writes a recipient memory
                           file.
  -b, --body <str>         Message body in ANY mode (alternative to the
                           positional arg).
  --body-file <path>       Read the body from a file ('-' = stdin); binds in
                           every mode.
  --ref-pr <int>           With --kind: PR number reference for triage.
  --ref-node <str>         With --kind: graph node id reference.
  --ref-gate <str>         With --kind: named gate/milestone reference.
  -J, --json               With --kind: print {msg_id, thread_path, appended}
                           as JSON.
  --raw                    Inject the payload UNWRAPPED at the recipient's
                           prompt line: one line, typed verbatim - a command
                           only: it must start with / or $. A status report or
                           any authored message goes WRAPPED (drop --raw) so
                           its sender stays visible. A payload starting /fno:
                           or $fno: is an fno command, and the lane rewrites
                           the marker to the form of the receiving harness. A
                           showing prompt is answered with `fno agents ask`.
                           Never queues durable. An actor OTHER than the model
                           must supply the trigger; self-injection is barred
                           unless --to-self. Mechanics and the reviewer-off-
                           the-author rationale: docs/architecture/review-
                           lanes.md.
  --check                  With --raw: report whether an injection path EXISTS
                           and inject nothing: 'injectable: <lane>' (exit 0),
                           'not-injectable' (1), 'unmeasurable' (3), malformed
                           payload (2). Reports a PATH only.
  --to-self                Address this session as the recipient (no <id>
                           needed); with --raw the envelope is stripped so a
                           slash command parses at your own prompt line.
  -F, --force              Deliver over the PANE transport: type the wrapped
                           body at the recipient's prompt, keeping every mail
                           semantic. The receipt says `typed (pane <id>)`,
                           NEVER `delivered` - bytes written to a PTY is not
                           action. It reads the pane first and refuses a
                           showing prompt.
  --style-exception <str>  Bypass the style check for this body with a stated
                           reason.
  --help                   Show this message and exit.
";

const REPLY_HELP: &str = "\
Usage: fno agents mail reply --to MSG_ID [OPTIONS] [BODY]

  Reply to a message, routed by the answered message's lane.

Options:
  --to <id>                The message being answered.
  --body <str>             Reply body (alternative to the positional arg).
  --body-file <path>       Read the body from a file ('-' = stdin).
  --kind <str>             Inbox kind of the reply.
  --from <project>         Sender project.
  --sender-session <id>    Sender session.
  --style-exception <str>  Bypass the style check for this body with a stated
                           reason.
  -J, --json               Print the receipt as JSON.
  --help                   Show this message and exit.
";

fn print_help() {
    println!("fno-agents mail - the do-not-disturb hold, team broadcast, sent view, withdraw, unread listing, ack, raw prompt-line send, reply, and drain-self for agent mail");
    println!("  hold [--minutes N | --for N] [--status] [--off]   alias: dnd");
    println!("  hold-release --handle KEY [--poll-s S]");
    println!("  seed-provenance                                 payload from FNO_SEED_PROV_* env");
    println!("  team --scope <all|leads|<role>|project:<p>> <message>");
    println!("  withdraw MSG_ID [--from-name NAME]");
    println!("  sent [--unclaimed] [--json] [--from-name LABEL]");
    println!("  pane-prepare --session-id S --pane N [--harness H] [--style-exception R]");
    println!("  send [--check] [--self-ok] [--review-request] [--style-exception <r>] [--origin <o>] <name> <payload>   raw prompt-line lane");
    println!(
        "  reply --to MSG_ID [BODY]                        routed by the answered message's lane"
    );
    println!("  unread [--name NAME | -n NAME] [--json | -J]");
    println!("  ack MSG_ID [--name NAME]                        alias front: fno mail ack");
    println!("  drain-self [--json]");
}
