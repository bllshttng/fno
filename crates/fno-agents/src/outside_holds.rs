//! Outside holds: CI check rows whose failure no commit on this repo can
//! heal. A row's fix lands in another repo or deploy, so a red on one is
//! external truth the way a merge freeze is: it must keep the PR red and
//! the promise blocked, but it must not cancel the PR's own runs, wake the
//! pusher with fix advice, or refuse a `<watching>` idle. Three readers
//! share this one list: the ci-cadence daemon (`ci_cadence::decide`), the
//! wait's cancel path (`pr_status::wait::cancel_in_flight`), and the stop
//! gate's classifier (`loopcheck::async_wait_class`).

/// Install-channel rows freed only by an fno-web Worker deploy.
/// `fno-sh-fresh` compares the bytes fno.sh serves against the newest
/// stable tag; the served bytes follow the Worker's INSTALL_SCRIPT_URL pin,
/// which lives in the fno-web repo. Every release that publishes a new tag
/// re-stales the pin until someone bumps and redeploys it, so the row reds
/// every open PR that touches an install surface while the pin lags - a
/// hold no PR-side commit can clear.
pub const CHECKS: &[&str] = &["fno-sh-fresh"];

/// Whether a failing check name names an outside hold. GitHub renders a
/// matrix leg as "{job} ({row-id})", so the row id is matched as a
/// substring, and a suffixed macOS leg of the same row matches too.
pub fn is_outside_hold(name: &str) -> bool {
    CHECKS.iter().any(|row| name.contains(row))
}

/// Whether at least one name is present and EVERY name is an outside hold:
/// the red is external and nothing on this head is debuggable. A real
/// failing check beside the hold keeps this false - that red is work.
pub fn all_outside_holds(names: &[String]) -> bool {
    !names.is_empty() && names.iter().all(|n| is_outside_hold(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_matching_covers_matrix_names_and_refuses_a_real_check_beside_the_hold() {
        assert!(is_outside_hold("fno-sh-fresh"));
        assert!(is_outside_hold("channel (fno-sh-fresh)"));
        assert!(is_outside_hold("fno-sh-fresh-macos"));
        assert!(!is_outside_hold("cargo-test"));
        assert!(!is_outside_hold("fno-sh-served"));
        let held = vec!["channel (fno-sh-fresh)".to_string()];
        assert!(all_outside_holds(&held));
        assert!(!all_outside_holds(&[]));
        let mixed = vec![
            "channel (fno-sh-fresh)".to_string(),
            "cargo-test".to_string(),
        ];
        assert!(!all_outside_holds(&mixed));
    }
}
