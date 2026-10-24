//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]), the managed slot stamp ([slot]), the proven
//! identity behind a launch's credential ([binding]), the staged dir layout
//! ([staging]), the launch env ([env]), the managed store with its shared
//! credential slot ([store]), the provider error taxonomy ([error_class]),
//! the runtime-state file ([state]), the usage probes ([usage_probe]), the
//! usage report ([usage]), the launch pick ([pick]) and the config write
//! ([writer]).
//! Ported from `fno.adapters.providers` (model.py, loader.py, binding.py,
//! staging.py, dispatch.py, error_taxonomy.py, runtime_state.py, usage.py
//! and managed.py's store layer and switch) and
//! `account_env.resolve_account_overlay`; the parity cases live in
//! `tests/accounts_*_parity.rs`.
//!
//! `fno-agents spawn-overlay accounts <command>` is where the
//! `fno config accounts` commands land: [show], [add], [doctor], [reconcile], [register], [remove], [usage], [pick], [use_switch], [launch_check], [combos] and [window] so far. They nest
//! under `spawn-overlay` so they add no fno-agents root action. `state <op>`
//! is the runtime-state writer the Python transports call ([state::run]), and
//! `switch` is the managed switch recovery's sweep transports ([use_switch::run_switch]).
pub mod add;
pub mod binding;
pub mod combos;
pub mod doctor;
pub mod env;
pub mod error_class;
pub mod launch_check;
pub mod pick;
pub mod reconcile;
pub mod records;
pub mod register;
pub mod remove;
pub mod show;
pub mod slot;
pub mod staging;
pub mod state;
pub mod store;
pub mod usage;
pub mod usage_probe;
pub mod use_switch;
pub mod window;
pub mod writer;

const USAGE: &str = "usage: fno-agents spawn-overlay accounts show <id> [--print-binding]\n       fno-agents spawn-overlay accounts add <id> -H <harness> -a <auth> [OPTIONS]\n       fno-agents spawn-overlay accounts doctor [--json]\n       fno-agents spawn-overlay accounts reconcile-slot [<harness>]\n       fno-agents spawn-overlay accounts register <id> [-H <harness>] [--config-dir <path>] [OPTIONS]\n       fno-agents spawn-overlay accounts remove <id> [--force] [--scope global|project]\n       fno-agents spawn-overlay accounts usage [--refresh] [--json]\n       fno-agents spawn-overlay accounts pick [--combo <name>] [--exclude <id>]... [--json] [--print-env] [--if-armed]\n       fno-agents spawn-overlay accounts use <id> [--scope project|global]\n       fno-agents spawn-overlay accounts combos <add|list|remove|test|use|update> [ARGS]\n       fno-agents spawn-overlay accounts window [--json] [--warn-seconds N]\n       fno-agents spawn-overlay accounts switch  (a JSON payload on stdin)\n       fno-agents spawn-overlay accounts test <id> [--smoke]\n       fno-agents spawn-overlay accounts state <op>  (a JSON payload on stdin)";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("show") => show::run(&args[1..]),
        Some("add") => add::run(&args[1..]),
        Some("doctor") => doctor::run(&args[1..]),
        Some("reconcile-slot") => reconcile::run(&args[1..]),
        Some("register") => register::run(&args[1..]),
        Some("remove") => remove::run(&args[1..]),
        Some("state") => state::run(&args[1..]),
        Some("usage") => usage::run(&args[1..]),
        Some("pick") => pick::run(&args[1..]),
        Some("test") => launch_check::run(&args[1..]),
        Some("use") => use_switch::run(&args[1..]),
        Some("switch") => use_switch::run_switch(&args[1..]),
        Some("combos") => combos::run(&args[1..]),
        Some("window") => window::run(&args[1..]),
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
