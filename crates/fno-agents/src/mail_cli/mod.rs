//! The `mail` front verbs on the Rust leg: the do-not-disturb hold, its
//! release timer, the `team` broadcast, the outbound sent view, and the
//! unread listing. Dispatch only; the verbs live in [hold], [team], [sent]
//! and [unread].
pub mod hold;
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
        "team" => team::run_team(&rest[1..]),
        "sent" => sent::run_sent(&rest[1..]),
        "unread" => unread::run(&rest[1..]),
        other => {
            eprintln!(
                "fno-agents mail: unknown verb {other:?} (hold | dnd | hold-release | team | sent | unread)"
            );

            2
        }
    }
}

fn print_help() {
    println!("fno-agents mail - the do-not-disturb hold, team broadcast, sent view and unread listing for agent mail");
    println!("  hold [--minutes N | --for N] [--status] [--off]   alias: dnd");
    println!("  hold-release --handle KEY [--poll-s S]");
    println!("  team --scope <all|leads|<role>|project:<p>> <message>");
    println!("  sent [--unclaimed] [--json] [--from-name LABEL]");
    println!("  unread [--name NAME | -n NAME] [--json | -J]");
}
