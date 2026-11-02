//! The `pr-watch` command family's Rust home. `status`, `install`, `refresh`,
//! `uninstall`, `heal` and `tick` all live here; the wheel keeps one forward
//! leaf so every spelling still answers.

mod clock;
mod discover;
mod dispatch;
mod heal;
mod install;
mod mint;
pub(crate) mod refresh;
mod render;
mod sessions;
mod state;
mod status;
mod tick;

use std::path::Path;

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("refresh") => refresh::run(&args[1..]),
        Some("status") => status::run(&args[1..]),
        Some("install") => install::run(&args[1..]),
        Some("uninstall") => install::run_uninstall(&args[1..]),
        Some("heal") => heal::run(&args[1..]),
        Some("tick") => tick::run(&args[1..]),
        Some("pr-sessions") => sessions::run(&args[1..]),
        Some("merge-cleanup-mint") => mint::run(&args[1..]),
        Some("ritual") => crate::post_merge::ritual::run(&args[1..]),
        Some("post-merge-dispatch") => crate::post_merge::route::run(&args[1..]),
        Some("--help" | "-h") => {
            println!(
                "usage: fno do pr watch <status|install|refresh|uninstall|heal|tick> [--json|-J] [-N]"
            );
            0
        }
        _ => {
            eprintln!(
                "usage: fno-agents pr-watch <status|install|refresh|uninstall|heal|tick|pr-sessions|merge-cleanup-mint|ritual|post-merge-dispatch> [--json|-J] [-N]"
            );
            2
        }
    }
}

/// Test seam for the refresh parity fixture: the fixture pre-renders the
/// plist with the renderer itself, so the "unchanged" case exercises the
/// no-change branch against the exact bytes the command writes.
#[doc(hidden)]
pub fn render_plist_for_test(
    launch_agents_dir: &Path,
    fno_binary: &str,
    install_path: Option<&str>,
    interval: i64,
) -> String {
    refresh::render_plist(launch_agents_dir, fno_binary, install_path, interval)
}
