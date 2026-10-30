//! A sandbox pinned by `FNO_CONFIG` alone never opens a share connection:
//! the share posture resolves from the pinned file, so the machine global's
//! `store.share_backlog = true` stays invisible to it. A docs screenshot
//! sandbox once synced 4,976 live rows because the global lookup skipped the
//! pin and read `~/.fno`. Integration build on purpose: the lib's cfg(test)
//! stub answers `Ok(None)` unconditionally and would test nothing.

use std::fs;

/// The env this test mutates is process-global; keep the file single-test so
/// no sibling test races the pins. (`share_backlog` also caches its answer in
/// a OnceLock for the process lifetime, which a second case in this binary
/// would read stale.)
#[test]
fn a_sandbox_pinned_by_fno_config_alone_never_shares() {
    let tmp = tempfile::tempdir().unwrap();
    // The machine global with share ON: the posture a sandbox must not
    // inherit.
    let global = tmp.path().join("home/.fno");
    fs::create_dir_all(&global).unwrap();
    fs::write(
        global.join("config.toml"),
        "[store]\nremote_url = \"http://127.0.0.1:1\"\nshare_backlog = true\n",
    )
    .unwrap();
    // The sandbox config: the documented recipe, no store keys at all.
    let sandbox = tmp.path().join("sandbox");
    fs::create_dir_all(&sandbox).unwrap();
    let pin = sandbox.join("config.toml");
    fs::write(&pin, "schema_version = 1\n").unwrap();

    let prior_config = std::env::var_os("FNO_CONFIG");
    let prior_global = std::env::var_os("FNO_GLOBAL_SETTINGS_PATH");
    std::env::set_var("FNO_CONFIG", &pin);
    std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", global.join("settings.yaml"));

    let shared = fno_agents::store_remote::share_backlog().unwrap();
    assert!(
        shared.is_none(),
        "the machine global's share posture leaked into a sandbox pinned by FNO_CONFIG alone"
    );

    match (prior_config, prior_global) {
        (Some(a), Some(b)) => {
            std::env::set_var("FNO_CONFIG", a);
            std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", b);
        }
        (Some(a), None) => {
            std::env::set_var("FNO_CONFIG", a);
            std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");
        }
        (None, Some(b)) => {
            std::env::remove_var("FNO_CONFIG");
            std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", b);
        }
        (None, None) => {
            std::env::remove_var("FNO_CONFIG");
            std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");
        }
    }
}
