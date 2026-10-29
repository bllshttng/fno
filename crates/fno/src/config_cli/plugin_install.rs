//! `fno config plugin install <harness> [flags]`: the dev-install door.
//!
//! The Python leg was a thin exec over `fno-agents plugin-install` for every
//! harness, codex included, so the native door claims every form and the
//! Python leg is gone. It keeps the captured usage line, the missing-binary
//! line and both help pages byte-identical.

use std::ffi::OsString;
use std::process::Command;

/// `fno config plugin [--help]`: the group page, or click's missing-command
/// refusal. Any other word is not a command of the group.
pub fn run_group(args: &[OsString]) -> i32 {
    let first = args.first().map(|a| a.to_string_lossy().to_string());
    match first.as_deref() {
        Some("--help") | Some("-h") => {
            print!("{GROUP_HELP}");
            0
        }
        None => group_usage_error("Missing command."),
        Some(word) if word.starts_with('-') => {
            group_usage_error(&format!("No such option: {word}"))
        }
        Some(word) => group_usage_error(&format!("No such command '{word}'.")),
    }
}

fn group_usage_error(message: &str) -> i32 {
    eprintln!("Usage: fno config plugin [OPTIONS] COMMAND [ARGS]...");
    eprintln!("Try 'fno config plugin --help' for help.");
    eprintln!();
    eprintln!("Error: {message}");
    2
}

/// The captured group page, byte for byte as typer printed it.
const GROUP_HELP: &str = concat!(
    "Usage: fno config plugin [OPTIONS] COMMAND [ARGS]...\n",
    "\n",
    "  Install your local checkout of footnote into a harness, for development\n",
    "\n",
    "Options:\n",
    "  --help  Show this message and exit.\n",
    "\n",
    "Commands:\n",
    "  install  Install your local checkout for development.\n",
);

pub fn run(args: &[OsString]) -> i32 {
    let tokens: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    for token in &tokens {
        if token == "--help" || token == "-h" {
            print!("{HELP}");
            return 0;
        }
    }
    if tokens.is_empty() {
        eprintln!("usage: fno config plugin install <claude|codex|opencode|agy> [flags...]");
        return 2;
    }
    let binary = crate::digest_overlay::fno_agents_bin();
    let child = Command::new(&binary)
        .arg("plugin-install")
        .args(&tokens)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(_) => {
            eprintln!(
                "fno config plugin install: the fno-agents binary was not found. \
Reinstall fno, run `fno doctor update --rust`, or set FNO_AGENTS_BIN."
            );
            return 2;
        }
    };
    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    }
}

/// The captured `--help` page, byte for byte as typer printed it.
const HELP: &str = concat!(
    "Usage: fno config plugin install [OPTIONS] [HARNESS]\n",
    "\n",
    "  Install your local checkout for development. --dev replaces a release-channel\n",
    "  plugin.\n",
    "\n",
    "  Side effects. It builds a stage under ~/.fno/plugin-stage/fno and points the\n",
    "  harness at it. It removes stale plugin copies. It strips the machine-wide\n",
    "  CARGO_BUILD_BUILD_DIR and SCCACHE_DIR lines an older install wrote, from\n",
    "  ~/.zshrc, ~/.bashrc, the Claude settings.json and the Codex config.toml. It\n",
    "  then runs a reclaim pass over fno's own caches, without the uv cache prune.\n",
    "  The receipt names every file it rewrote.\n",
    "\n",
    "Arguments:\n",
    "  [HARNESS]  claude | codex | opencode | agy\n",
    "\n",
    "Options:\n",
    "  --help  Show this message and exit.\n",
    "",
);
