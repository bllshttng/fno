//! `fno config accounts <command>`: the account commands `fno-agents spawn-overlay accounts`
//! answers natively. A command not listed here still forwards to the Python
//! group until its own port lands.

use std::ffi::OsString;

/// The account commands served by `fno-agents spawn-overlay accounts`.
pub const NATIVE_ACCOUNTS_COMMANDS: &[&str] = &[
    "show",
    "add",
    "reconcile-slot",
    "register",
    "remove",
    "usage",
    "doctor",
];

/// `fno config accounts <command> ...` for a native command: the args from the
/// command on, ready for `fno-agents spawn-overlay accounts`.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    let word = |i: usize| args.get(i).and_then(|a| a.to_str());
    if word(0) != Some("config") || word(1) != Some("accounts") {
        return None;
    }
    NATIVE_ACCOUNTS_COMMANDS
        .contains(&word(2)?)
        .then(|| args[2..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oss(pieces: &[&str]) -> Vec<OsString> {
        pieces.iter().map(OsString::from).collect()
    }

    #[test]
    fn classifies_native_account_commands_only() {
        let rest = classify(&oss(&[
            "config",
            "accounts",
            "show",
            "alt",
            "--print-binding",
        ]))
        .unwrap();
        assert_eq!(rest, oss(&["show", "alt", "--print-binding"]));
        let rest = classify(&oss(&["config", "accounts", "add", "alt", "-H", "claude"])).unwrap();
        assert_eq!(rest, oss(&["add", "alt", "-H", "claude"]));
        let rest = classify(&oss(&["config", "accounts", "reconcile-slot"])).unwrap();
        assert_eq!(rest, oss(&["reconcile-slot"]));
        let rest = classify(&oss(&["config", "accounts", "register", "alt"])).unwrap();
        assert_eq!(rest, oss(&["register", "alt"]));
        let rest = classify(&oss(&["config", "accounts", "remove", "alt", "--force"])).unwrap();
        assert_eq!(rest, oss(&["remove", "alt", "--force"]));
        let rest = classify(&oss(&["config", "accounts", "doctor", "-J"])).unwrap();
        assert_eq!(rest, oss(&["doctor", "-J"]));
        assert_eq!(classify(&oss(&["config", "accounts", "list"])), None);
        assert_eq!(classify(&oss(&["config", "accounts"])), None);
        assert_eq!(classify(&oss(&["config", "paths", "show"])), None);
    }
}
