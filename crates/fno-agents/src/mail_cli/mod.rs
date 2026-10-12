//! The `mail` front verbs on the Rust leg: the do-not-disturb hold leaf and
//! its release timer. Dispatch only; the verbs live in [hold].
pub mod hold;

pub fn run(rest: &[String]) -> i32 {
    let first = rest.first().map(String::as_str);
    if rest.is_empty() || first == Some("--help") || first == Some("-h") {
        print_help();
        return if rest.is_empty() { 2 } else { 0 };
    }
    match rest[0].as_str() {
        "hold" | "dnd" => hold::run_hold(&rest[1..]),
        "hold-release" => hold::run_hold_release(&rest[1..]),
        other => {
            eprintln!("fno-agents mail: unknown verb {other:?} (hold | dnd | hold-release)");
            2
        }
    }
}

fn print_help() {
    println!("fno-agents mail - the do-not-disturb (DND) hold for agent mail");
    println!("  hold [--minutes N | --for N] [--status] [--off]   alias: dnd");
    println!("  hold-release --handle KEY [--poll-s S]");
}
