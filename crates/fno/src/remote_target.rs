//! The front door's cross-host intercept.
//!
//! When a positional target carries `@`, the invocation is cross-host:
//! `agents mail send <name>@<host> ...` and `agents peek <handle>@<host>
//! ...` exec the sibling `fno-agents mail-remote` verb. Every other argv
//! forwards byte-verbatim, so the Python verbs change nothing.

use std::ffi::OsString;

/// The full argv, `agents` leading. The claimed tail runs from the target
/// onward; `None` forwards byte-verbatim.
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
            tail = args[3..].to_vec();
        }
        "peek" => {
            let target = args.get(2)?.to_str()?;
            if !target.contains('@') {
                return None;
            }
            tail = args[2..].to_vec();
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
    fn mail_send_target_with_at_is_claimed() {
        let argv = classify(&os(&["agents", "mail", "send", "nemo@mbp3", "hello"]))
            .expect("@host send is claimed");
        assert_eq!(argv, os(&["send", "nemo@mbp3", "hello"]));
    }

    #[test]
    fn peek_target_with_at_is_claimed() {
        let argv = classify(&os(&["agents", "peek", "nemo@mbp3", "-n", "5"]))
            .expect("@host peek is claimed");
        assert_eq!(argv, os(&["peek", "nemo@mbp3", "-n", "5"]));
    }

    #[test]
    fn bare_targets_forward() {
        assert!(classify(&os(&["agents", "mail", "send", "nemo", "hello"])).is_none());
        assert!(classify(&os(&["agents", "peek", "nemo"])).is_none());
    }

    #[test]
    fn other_agents_verbs_forward() {
        assert!(classify(&os(&["agents", "claim", "status"])).is_none());
        assert!(classify(&os(&["agents", "mail", "list"])).is_none());
    }
}
