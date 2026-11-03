//! `fno config accounts <command>`: every spelling lands on
//! `fno-agents spawn-overlay accounts`, which answers the commands below, the
//! bare group and `--help` with its usage, and refuses an unknown word by name.

use std::ffi::OsString;

/// The account commands served by `fno-agents spawn-overlay accounts`.
pub const NATIVE_ACCOUNTS_COMMANDS: &[&str] = &[
    "show",
    "add",
    "reconcile-slot",
    "register",
    "remove",
    "usage",
    "pick",
    "test",
    "doctor",
    "list",
    "use",
    "combos",
    "window",
    "benchmarks",
    "required-bot-check",
];

/// `fno config accounts ...`: the args from the command on, ready for
/// `fno-agents spawn-overlay accounts`.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    let word = |i: usize| args.get(i).and_then(|a| a.to_str());
    (word(0) == Some("config") && word(1) == Some("accounts")).then(|| args[2..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oss(pieces: &[&str]) -> Vec<OsString> {
        pieces.iter().map(OsString::from).collect()
    }

    #[test]
    fn classifies_every_config_accounts_spelling() {
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
        let rest = classify(&oss(&["config", "accounts", "test", "alt", "--smoke"])).unwrap();
        assert_eq!(rest, oss(&["test", "alt", "--smoke"]));
        let rest = classify(&oss(&["config", "accounts", "combos", "list", "-J"])).unwrap();
        assert_eq!(rest, oss(&["combos", "list", "-J"]));
        let rest = classify(&oss(&["config", "accounts", "window", "-J"])).unwrap();
        assert_eq!(rest, oss(&["window", "-J"]));
        let rest = classify(&oss(&["config", "accounts", "required-bot-check", "-J"])).unwrap();
        assert_eq!(rest, oss(&["required-bot-check", "-J"]));
        let rest = classify(&oss(&["config", "accounts", "doctor", "-J"])).unwrap();
        assert_eq!(rest, oss(&["doctor", "-J"]));
        let rest = classify(&oss(&["config", "accounts", "list", "-J"])).unwrap();
        assert_eq!(rest, oss(&["list", "-J"]));
        let rest = classify(&oss(&[
            "config", "accounts", "use", "alt", "--scope", "global",
        ]))
        .unwrap();
        assert_eq!(rest, oss(&["use", "alt", "--scope", "global"]));
        let rest = classify(&oss(&["config", "accounts", "benchmarks", "show"])).unwrap();
        assert_eq!(rest, oss(&["benchmarks", "show"]));
        assert_eq!(classify(&oss(&["config", "accounts"])), Some(vec![]));
        assert_eq!(
            classify(&oss(&["config", "accounts", "--help"])),
            Some(oss(&["--help"]))
        );
        assert_eq!(
            classify(&oss(&["config", "accounts", "bogus"])),
            Some(oss(&["bogus"]))
        );
        assert_eq!(classify(&oss(&["config", "paths", "show"])), None);
    }
}
