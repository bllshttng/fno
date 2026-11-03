//! Where a message goes: the four address forms and the host spelling rule.
//! Resolving a handle to a live registry row is not this module's job.

/// A parsed destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    /// A bare name or session id on this machine.
    Handle(String),
    /// `name@host`, with the host already in `normalize_host` form.
    Remote { name: String, host: String },
    /// `--to-lead <scope>`: whoever holds the lead role for the scope.
    Lead(String),
    /// `--to-project <project>`: the one live peer that answers for it.
    Project(String),
}

impl Address {
    /// Parse the positional target: a handle, or `name@host`.
    pub fn parse_target(target: &str) -> Result<Address, String> {
        if !target.contains('@') {
            return Ok(Address::Handle(target.to_string()));
        }
        let (name, host) = split_target(target)?;
        Ok(Address::Remote {
            name,
            host: normalize_host(&host),
        })
    }
}

/// What `split_target` refused.
pub fn target_error(target: &str) -> String {
    format!(
        "cross-host target {target:?}: the form is <name>@<host>, one @, \
both sides nonempty; a host alias in ssh_config covers user@host spellings"
    )
}

/// Split `<name>@<host>` at its single `@`. Both sides nonempty; two `@`
/// refuse (an ssh user@host spelling is not carried; an ssh_config Host
/// alias covers those).
pub fn split_target(target: &str) -> Result<(String, String), String> {
    let at_count = target.matches('@').count();
    if at_count != 1 {
        return Err(target_error(target));
    }
    let (name, host) = target.split_once('@').unwrap_or(("", ""));
    if name.is_empty() || host.is_empty() {
        return Err(target_error(target));
    }
    Ok((name.to_string(), host.to_string()))
}

/// This machine's bus host: the stable `host_label()` (macOS `LocalHostName`),
/// normalized. `gethostname(2)` drifts with DHCP, VPN and sleep, which would
/// strand pending rows addressed to the old spelling. Pull and the sender
/// stamp both read this one function, so they cannot disagree.
pub fn local_host() -> String {
    normalize_host(&crate::name_ledger::host_label())
}

/// Hosts compare equal across machines that spell themselves differently
/// (`iMac2D2.local` from gethostname, `imac2d2` from a user typing). One
/// normalizer runs on write and on pull (operator ruling, 2026-10-10).
pub fn normalize_host(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.strip_suffix(".local") {
        Some(base) if !base.is_empty() => base.to_string(),
        _ => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_host_is_the_normalized_stable_label() {
        let label = crate::name_ledger::host_label();
        assert_eq!(local_host(), normalize_host(&label));
        assert!(!local_host().ends_with(".local"));
    }
}
