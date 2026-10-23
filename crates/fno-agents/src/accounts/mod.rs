//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]), the managed slot stamp ([slot]), the proven
//! identity behind a launch's credential ([binding]), the staged dir layout
//! ([staging]), the launch env ([env]), the managed store with its shared
//! credential slot ([store]), the provider error taxonomy ([error_class]),
//! the runtime-state file ([state]), the usage probes ([usage_probe]) and
//! the config write ([writer]).
//! Ported from `fno.adapters.providers` (model.py, loader.py, binding.py,
//! staging.py, dispatch.py, error_taxonomy.py, runtime_state.py, usage.py
//! and managed.py's store layer) and
//! `account_env.resolve_account_overlay`; the parity cases live in
//! `tests/accounts_*_parity.rs`.
//!
//! `fno-agents spawn-overlay accounts <command>` is where the
//! `fno config accounts` commands land: [show], [add] and [reconcile] so far. They nest
//! under `spawn-overlay` so they add no fno-agents root action. `state <op>`
//! is the runtime-state writer the Python transports call ([state::run]).
pub mod add;
pub mod binding;
pub mod env;
pub mod error_class;
pub mod reconcile;
pub mod records;
pub mod show;
pub mod slot;
pub mod staging;
pub mod state;
pub mod store;
pub mod usage_probe;
pub mod writer;

const USAGE: &str = "usage: fno-agents spawn-overlay accounts show <id> [--print-binding]\n       fno-agents spawn-overlay accounts add <id> -H <harness> -a <auth> [OPTIONS]\n       fno-agents spawn-overlay accounts reconcile-slot [<harness>]\n       fno-agents spawn-overlay accounts state <op>  (a JSON payload on stdin)";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("show") => show::run(&args[1..]),
        Some("add") => add::run(&args[1..]),
        Some("reconcile-slot") => reconcile::run(&args[1..]),
        Some("state") => state::run(&args[1..]),
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
