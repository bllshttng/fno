//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]), the managed slot stamp ([slot]), the proven
//! identity behind a launch's credential ([binding]), the staged dir layout
//! ([staging]), the launch env and `--account` overlay ([env]), the spawn's
//! account answers ([launch]) and the one credential composition
//! ([credentials]). Ported from `fno.adapters.providers` (model.py, the read
//! side of loader.py, binding.py, staging.py, dispatch.py and the managed.py
//! reads); the parity cases live in `tests/accounts_*_parity.rs`.
//!
//! `fno-agents spawn-overlay accounts <command>` is where the
//! `fno config accounts` commands land: [show] so far. They nest under
//! `spawn-overlay` so they add no fno-agents root action.
pub mod binding;
pub mod credentials;
pub mod env;
pub mod launch;
pub mod records;
pub mod show;
pub mod slot;
pub mod staging;

const USAGE: &str = "usage: fno-agents spawn-overlay accounts show <id> [--print-binding]";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("show") => show::run(&args[1..]),
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
