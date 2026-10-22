//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]), the managed slot stamp ([slot]), the proven
//! identity behind a launch's credential ([binding]), the staged dir layout
//! ([staging]) and the launch env ([env]). Ported from
//! `fno.adapters.providers` (model.py, the read side of loader.py,
//! binding.py, staging.py, dispatch.py and the managed.py reads) and
//! `account_env.resolve_account_overlay`; the parity cases live in
//! `tests/accounts_*_parity.rs`.
//!
//! `fno-agents accounts <verb>` is the group the `fno config accounts`
//! verbs land on: [show] so far, and `fallback-chain`, the failover chain
//! walk over a stdin payload (see fallback_chain.rs).
pub mod binding;
pub mod env;
pub mod records;
pub mod show;
pub mod slot;
pub mod staging;

const USAGE: &str = "usage: fno-agents accounts <show|fallback-chain> [args]";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("show") => show::run(&args[1..]),
        Some("fallback-chain") => crate::fallback_chain::run_fallback_chain(&args[1..]),
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
