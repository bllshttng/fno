//! Preserve client panics while the alternate screen owns the terminal.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Once,
};

static PANICKED: AtomicBool = AtomicBool::new(false);

pub(super) fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            PANICKED.store(true, Ordering::Release);
            crate::mux_log::client(
                "error",
                &format!(
                    "panic: {info}\nbacktrace:\n{}",
                    std::backtrace::Backtrace::force_capture()
                ),
            );
            crate::mux_log::flush(&crate::mux_log::client_path());
        }))
    });
}

pub(super) fn after_restore() {
    crate::mux_log::flush(&crate::mux_log::client_path());
    if PANICKED.swap(false, Ordering::AcqRel) {
        eprintln!(
            "fno: client failed; see {} (report: prefix+!)",
            crate::mux_log::client_path().display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mux_log_client_panic_is_kept_and_reported_only_after_restore() {
        const NAME: &str = "client::panic_log::tests::mux_log_client_panic_is_kept_and_reported_only_after_restore";
        if let Some(observation) = std::env::var_os("FNO_MUX_PANIC_PROBE") {
            let log = crate::mux_log::client_path();
            std::fs::create_dir_all(log.parent().unwrap()).unwrap();
            install();
            let result = std::panic::catch_unwind(|| panic!("mux panic probe"));
            assert!(result.is_err());
            after_restore();
            std::fs::copy(log, observation).unwrap();
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env("FNO_MUX_PANIC_PROBE", dir.path().join("observed.log"))
            .env("FNO_MUX_DIR", dir.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let path = dir.path().join("observed.log");
        assert!(path.exists(), "the panic must survive in client.log");
        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("mux panic probe"));
        assert!(log.contains("panic_log.rs:"));
        assert!(log.contains("backtrace:"));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.lines().count(),
            1,
            "one notice after restore: {stderr}"
        );
        assert!(stderr.contains("client.log"));
        assert!(stderr.contains("prefix+!"));
    }
}
