//! The front door's cross-host intercept.
//!
//! When a positional target carries `@`, the invocation is cross-host:
//! `agents mail send <name>@<host> ...` and `agents peek <handle>@<host>
//! ...` exec the sibling `fno-agents mail-remote` verb. Every other argv
//! forwards byte-verbatim, so the Python verbs change nothing.

use std::ffi::OsString;

/// The full argv, `agents` leading. The claimed tail runs from the verb
/// onward (`send|peek` first), the shape `mail_remote::run` routes on;
/// `None` forwards byte-verbatim.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.first()?.to_str()? != "agents" {
        return None;
    }
    let tail: Vec<OsString>;
    match args.get(1)?.to_str()? {
        "mail" => {
            if !matches!(args.get(2)?.to_str()?, "send" | "peek") {
                return None;
            }
            let target = args.get(3)?.to_str()?;
            if !target.contains('@') {
                return None;
            }
            tail = args[2..].to_vec();
        }
        "peek" => {
            let target = args.get(2)?.to_str()?;
            if !target.contains('@') {
                return None;
            }
            tail = args[1..].to_vec();
        }
        _ => return None,
    }
    Some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn classify_claims_only_at_host_targets() {
        // A mail send with an @host target is claimed; the tail runs from
        // the verb onward, the shape mail_remote::run routes on.
        let argv = classify(&os(&["agents", "mail", "send", "nemo@mbp3", "hello"]))
            .expect("@host send is claimed");
        assert_eq!(argv, os(&["send", "nemo@mbp3", "hello"]));
        // A peek with an @host handle is claimed.
        let argv = classify(&os(&["agents", "peek", "nemo@mbp3", "-n", "5"]))
            .expect("@host peek is claimed");
        assert_eq!(argv, os(&["peek", "nemo@mbp3", "-n", "5"]));
        // Bare targets forward byte-verbatim.
        assert!(classify(&os(&["agents", "mail", "send", "nemo", "hello"])).is_none());
        assert!(classify(&os(&["agents", "peek", "nemo"])).is_none());
        // Other agents verbs and other mail subcommands forward.
        assert!(classify(&os(&["agents", "claim", "status"])).is_none());
        assert!(classify(&os(&["agents", "mail", "list"])).is_none());
    }
}
