//! Native `fno doctor lint` checks for the front door. `style` runs in the
//! sibling fno-agents binary's hidden `style-check` command; `menu-caps` runs
//! in-process here (crate::menu_caps), because no new command may land on
//! fno-agents. Every other `doctor lint` check keeps forwarding to the
//! Python CLI, and the classified argv passes on byte-verbatim.

use std::ffi::OsString;

/// A `doctor lint` check the front door answers itself, with the flags
/// AFTER the check name (the classifier strips the whole classified prefix).
#[derive(Debug, PartialEq, Eq)]
pub enum NativeLint {
    Style(Vec<OsString>),
    MenuCaps(Vec<OsString>),
}

/// Classify `fno doctor lint <check> ...`: `Some` for a native check,
/// `None` forwards to the Python CLI.
pub fn classify_doctor_lint(args: &[OsString]) -> Option<NativeLint> {
    if args.len() < 3 {
        return None;
    }
    if args[0].to_str()? != "doctor" || args[1].to_str()? != "lint" {
        return None;
    }
    let rest = args[3..].to_vec();
    match args[2].to_str()? {
        "style" => Some(NativeLint::Style(rest)),
        "menu-caps" => Some(NativeLint::MenuCaps(rest)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn classify_admits_only_the_native_checks() {
        assert_eq!(
            classify_doctor_lint(&mk(&["doctor", "lint", "style", "--stdin"])),
            Some(NativeLint::Style(mk(&["--stdin"])))
        );
        assert_eq!(
            classify_doctor_lint(&mk(&["doctor", "lint", "menu-caps"])),
            Some(NativeLint::MenuCaps(mk(&[])))
        );
        // Every other check keeps forwarding to the Python CLI.
        assert!(classify_doctor_lint(&mk(&["doctor", "lint", "command-ratchet"])).is_none());
        assert!(classify_doctor_lint(&mk(&["doctor", "lint"])).is_none());
        // A different tree keeps forwarding.
        assert!(classify_doctor_lint(&mk(&["doctor", "event", "rows"])).is_none());
        assert!(classify_doctor_lint(&mk(&["doctor"])).is_none());
        assert!(classify_doctor_lint(&mk(&[])).is_none());
    }
}
