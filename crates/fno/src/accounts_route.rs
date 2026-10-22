//! `fno config accounts <verb>`: the account verbs `fno-agents spawn-overlay accounts`
//! answers natively. A verb not listed here still forwards to the Python
//! group until its own port lands.

use std::ffi::OsString;

/// The account verbs served by `fno-agents spawn-overlay accounts`.
pub const NATIVE_ACCOUNTS_VERBS: &[&str] = &["show"];

/// `fno config accounts <verb> ...` for a native verb: the args from the
/// verb on, ready for `fno-agents spawn-overlay accounts`.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    let word = |i: usize| args.get(i).and_then(|a| a.to_str());
    if word(0) != Some("config") || word(1) != Some("accounts") {
        return None;
    }
    NATIVE_ACCOUNTS_VERBS
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
    fn classifies_native_account_verbs_only() {
        let rest = classify(&oss(&[
            "config",
            "accounts",
            "show",
            "alt",
            "--print-binding",
        ]))
        .unwrap();
        assert_eq!(rest, oss(&["show", "alt", "--print-binding"]));
        assert_eq!(classify(&oss(&["config", "accounts", "list"])), None);
        assert_eq!(classify(&oss(&["config", "accounts"])), None);
        assert_eq!(classify(&oss(&["config", "paths", "show"])), None);
    }
}
