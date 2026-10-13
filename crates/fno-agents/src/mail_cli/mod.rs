//! The `mail` front verbs on the Rust leg: the do-not-disturb hold, its
//! release timer, the session seed-provenance leaf, the `team` broadcast,
//! the outbound sent view, the unread listing, the ack leaf, and the raw
//! prompt-line send. Dispatch only; the verbs live in [hold],
//! [seed_provenance], [team], [sent], [unread], [ack], and [send_raw].
pub mod ack;
pub mod hold;
pub mod seed_provenance;
pub mod send_raw;
pub mod sent;
pub mod team;
pub mod unread;

pub fn run(rest: &[String]) -> i32 {
    let first = rest.first().map(String::as_str);
    if rest.is_empty() || first == Some("--help") || first == Some("-h") {
        print_help();
        return if rest.is_empty() { 2 } else { 0 };
    }
    match rest[0].as_str() {
        "hold" | "dnd" => hold::run_hold(&rest[1..]),
        "hold-release" => hold::run_hold_release(&rest[1..]),
        "seed-provenance" => seed_provenance::run(&rest[1..]),
        "team" => team::run_team(&rest[1..]),
        "sent" => sent::run_sent(&rest[1..]),
        "send" => send_raw::run(&rest[1..]),
        "unread" => unread::run(&rest[1..]),
        "ack" => ack::run_ack(&rest[1..]),
        other => {
            eprintln!(
                "fno-agents mail: unknown verb {other:?} (hold | dnd | hold-release | seed-provenance | team | sent | send | unread | ack)"
            );

            2
        }
    }
}

fn print_help() {
    println!("fno-agents mail - the do-not-disturb hold, team broadcast, sent view, unread listing and ack leaf for agent mail");
    println!("  hold [--minutes N | --for N] [--status] [--off]   alias: dnd");
    println!("  hold-release --handle KEY [--poll-s S]");
    println!("  seed-provenance                                 payload from FNO_SEED_PROV_* env");
    println!("  team --scope <all|leads|<role>|project:<p>> <message>");
    println!("  sent [--unclaimed] [--json] [--from-name LABEL]");
    println!("  send [--check] [--self-ok] [--review-request] [--style-exception <r>] [--origin <o>] <name> <payload>   raw prompt-line lane");
    println!("  unread [--name NAME | -n NAME] [--json | -J]");
    println!("  ack MSG_ID [--name NAME]                        alias front: fno mail ack");
}
