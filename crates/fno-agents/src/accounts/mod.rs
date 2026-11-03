//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]), the managed slot stamp ([slot]), the proven
//! identity behind a launch's credential ([binding]), the staged dir layout
//! ([staging]), the launch env ([env]), the managed store with its shared
//! credential slot ([store]), the provider error taxonomy ([error_class]),
//! the runtime-state file ([state]), the usage probes ([usage_probe]), the
//! usage report ([usage]), the launch pick ([pick]), the config write
//! ([writer]), the spawn's account answers ([launch]) and the one
//! credential composition ([credentials]).
//! Ported from the deleted Python adapters package (model.py, loader.py, binding.py,
//! staging.py, dispatch.py, error_taxonomy.py, runtime_state.py, usage.py
//! and managed.py's store layer and switch) and
//! `account_env.resolve_account_overlay`; the parity cases live in
//! `tests/accounts_*_parity.rs`.
//!
//! `fno-agents spawn-overlay accounts <command>` is where the
//! `fno config accounts` commands land: [show], [add], [doctor], [list], [reconcile], [register], [remove], [usage], [pick], [use_switch], [launch_check], [combos], [window], [benchmarks] and [required_bot] so far. They nest
//! under `spawn-overlay` so they add no fno-agents root action. `state <op>`
//! is the runtime-state writer the Python transports call ([state::run]),
//! `switch` is the managed switch recovery's sweep transports ([use_switch::run_switch]),
//! and `error-class` and `claude-dirs` answer the Python liveness reads.
pub mod add;
pub mod benchmarks;
pub mod binding;
pub mod combos;
pub mod credentials;
pub mod doctor;
pub mod env;
pub mod error_class;
pub mod launch;
pub mod launch_check;
pub mod list;
pub mod logins;
pub mod pick;
pub mod reconcile;
pub mod records;
pub mod register;
pub mod remove;
pub mod required_bot;
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

const USAGE: &str = "usage: fno-agents spawn-overlay accounts show <id> [--print-binding]\n       fno-agents spawn-overlay accounts add <id> -H <harness> -a <auth> [OPTIONS]\n       fno-agents spawn-overlay accounts doctor [--json]\n       fno-agents spawn-overlay accounts list [--json]\n       fno-agents spawn-overlay accounts reconcile-slot [<harness>]\n       fno-agents spawn-overlay accounts register <id> [-H <harness>] [--config-dir <path>] [OPTIONS]\n       fno-agents spawn-overlay accounts remove <id> [--force] [--scope global|project]\n       fno-agents spawn-overlay accounts usage [--refresh] [--logins] [--json]\n       fno-agents spawn-overlay accounts pick [--combo <name>] [--exclude <id>]... [--json] [--print-env] [--if-armed]\n       fno-agents spawn-overlay accounts use <id> [--scope project|global]\n       fno-agents spawn-overlay accounts combos <add|list|remove|test|use|update> [ARGS]\n       fno-agents spawn-overlay accounts window [--json] [--warn-seconds N]\n       fno-agents spawn-overlay accounts required-bot-check [--json]\n       fno-agents spawn-overlay accounts switch  (a JSON payload on stdin)\n       fno-agents spawn-overlay accounts test <id> [--smoke]\n       fno-agents spawn-overlay accounts benchmarks <refresh|show|tiers>\n       fno-agents spawn-overlay accounts state <op>  (a JSON payload on stdin)\n       fno-agents spawn-overlay accounts error-class  (a JSON payload on stdin)\n       fno-agents spawn-overlay accounts claude-dirs";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("show") => show::run(&args[1..]),
        Some("add") => add::run(&args[1..]),
        Some("doctor") => doctor::run(&args[1..]),
        Some("list") => list::run(&args[1..]),
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
        Some("benchmarks") => benchmarks::run(&args[1..]),
        Some("required-bot-check") => required_bot::run(&args[1..]),
        Some("error-class") => error_class_door(),
        Some("claude-dirs") => claude_dirs_door(),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("fno config accounts: unknown command: {other}\n{USAGE}");
            2
        }
        None => {
            eprintln!("{USAGE}");
            2
        }
    }
}

/// `error-class`: one outcome on stdin (`body`, and optionally `http_status`,
/// `exit_code`, `reset_timezone`, `model`, `parser_failed`), its
/// [`error_class::normalize`] answer on stdout. The Python liveness read
/// classes a worker's last turn through this door.
fn error_class_door() -> i32 {
    use serde_json::Value;
    use std::io::Read as _;
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("accounts error-class: stdin unreadable: {error}");
        return 2;
    }
    let Ok(payload @ Value::Object(_)) = serde_json::from_str::<Value>(&input) else {
        eprintln!("accounts error-class: the payload is not a JSON object");
        return 2;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let outcome = error_class::Outcome {
        http_status: payload["http_status"].as_i64(),
        exit_code: payload["exit_code"].as_i64(),
        body: payload["body"].as_str().unwrap_or(""),
        parser_failed: payload["parser_failed"].as_bool().unwrap_or(false),
        model: payload["model"].as_str(),
        reset_timezone: payload["reset_timezone"].as_str(),
        now,
    };
    match error_class::normalize(&outcome) {
        Ok(verdict) => {
            println!("{}", verdict.to_json());
            0
        }
        Err(error) => {
            eprintln!("accounts error-class: {error}");
            2
        }
    }
}

/// `claude-dirs`: the `config_dir` of every claude account record, read
/// through [`records::load`] so `$FNO_CONFIG` and the layer walk hold. An
/// unreadable config answers no dirs; the caller keeps its ambient roots.
fn claude_dirs_door() -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let dirs: Vec<String> = records::load(None, &cwd)
        .map(|a| a.records)
        .unwrap_or_default()
        .iter()
        .filter(|r| r.harness == "claude")
        .filter_map(|r| r.config_dir.as_ref())
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    println!("{}", serde_json::json!({ "dirs": dirs }));
    0
}
