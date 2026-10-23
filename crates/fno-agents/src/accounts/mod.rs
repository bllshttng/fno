//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]), the managed slot stamp ([slot]), the proven
//! identity behind a launch's credential ([binding]), the staged dir layout
//! ([staging]), the launch env ([env]), the managed store with its shared
//! credential slot ([store]) and the config write ([writer]). Ported from
//! `fno.adapters.providers` (model.py, loader.py, binding.py, staging.py,
//! dispatch.py and managed.py's store layer) and
//! `account_env.resolve_account_overlay`; the parity cases live in
//! `tests/accounts_*_parity.rs`.
//!
//! `fno-agents spawn-overlay accounts <command>` is where the
//! `fno config accounts` commands land: [show] and [add] so far. They nest
//! under `spawn-overlay` so they add no fno-agents root action.
pub mod add;
pub mod binding;
pub mod env;
pub mod records;
pub mod show;
pub mod slot;
pub mod staging;
pub mod store;
pub mod writer;

const USAGE: &str = "usage: fno-agents spawn-overlay accounts show <id> [--print-binding]\n       fno-agents spawn-overlay accounts add <id> -H <harness> -a <auth> [OPTIONS]";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("show") => show::run(&args[1..]),
        Some("add") => add::run(&args[1..]),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}
