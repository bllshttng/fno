//! The `mail` front verbs on the Rust leg: the do-not-disturb hold, its
//! release timer, the pane-send renderer, the session seed-provenance leaf,
//! the `team` broadcast, the outbound sent view, and the ack leaf.
//! Dispatch only; the verbs live in [hold], [pane_prepare], [seed_provenance],
//! [team], [sent], and [ack].
pub mod ack;
pub mod hold;
pub mod pane_prepare;
pub mod seed_provenance;
pub mod sent;
pub mod team;

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
        "pane-prepare" => pane_prepare::run(&rest[1..]),
        "ack" => ack::run_ack(&rest[1..]),
        other => {
            eprintln!(
                "fno-agents mail: unknown verb {other:?} (hold | dnd | hold-release | pane-prepare | team | sent | ack)"
            );
            2
        }
    }
}

fn print_help() {
    println!(
        "fno-agents mail - the do-not-disturb hold, team broadcast, and sent view for agent mail"
    );
    println!("  hold [--minutes N | --for N] [--status] [--off]   alias: dnd");
    println!("  hold-release --handle KEY [--poll-s S]");
    println!("  team --scope <all|leads|<role>|project:<p>> <message>");
    println!("  sent [--unclaimed] [--json] [--from-name LABEL]");
    println!("  pane-prepare --session-id S --pane N [--harness H] [--style-exception R]");
    println!("  ack MSG_ID [--name NAME]                        alias front: fno mail ack");
}
