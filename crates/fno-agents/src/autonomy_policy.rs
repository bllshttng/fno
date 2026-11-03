//! `autonomy.mode`: may this fleet action run without another user decision?
//!
//! This module is the one owner of that answer. Each door keeps its own
//! eligibility checks (CI, review, claims, holds); the mode only decides
//! whether the door may act unasked. An unreadable or malformed mode reads
//! `ask`, so a broken config can never widen authority.

use std::path::Path;

use serde_json::{json, Value};

use crate::agents_config::{config_candidates, parse_config};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ask,
    Balanced,
    Yolo,
}

impl Mode {
    fn word(self) -> &'static str {
        match self {
            Mode::Ask => "ask",
            Mode::Balanced => "balanced",
            Mode::Yolo => "yolo",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Restart,
    Merge,
    Law,
}

impl Action {
    fn word(self) -> &'static str {
        match self {
            Action::Restart => "restart",
            Action::Merge => "merge",
            Action::Law => "law",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The door acts unasked.
    Auto,
    /// The door defers to its own standing config (merge: `auto_merge.*`).
    ConfigDecides,
    /// The door holds until the user decides.
    Ask,
}

impl Disposition {
    fn word(self) -> &'static str {
        match self {
            Disposition::Auto => "auto",
            Disposition::ConfigDecides => "config",
            Disposition::Ask => "ask",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub mode: Mode,
    pub allow: Vec<Action>,
    pub source: String,
}

impl Policy {
    /// The ruled matrix, in one place.
    pub fn disposition(&self, action: Action) -> Disposition {
        let allowed = self.allow.contains(&action);
        match (self.mode, action) {
            (Mode::Ask, Action::Merge) if allowed => Disposition::ConfigDecides,
            (Mode::Ask, _) if allowed => Disposition::Auto,
            (Mode::Ask, _) => Disposition::Ask,
            (Mode::Balanced, Action::Restart) => Disposition::Auto,
            (Mode::Balanced, Action::Merge) => Disposition::ConfigDecides,
            (Mode::Balanced, Action::Law) => Disposition::Ask,
            (Mode::Yolo, _) => Disposition::Auto,
        }
    }
}

fn fail_closed(source: String) -> Policy {
    Policy {
        mode: Mode::Ask,
        allow: Vec::new(),
        source,
    }
}

/// The first candidate carrying a leaf decides that leaf, the precedence
/// `auto_merge_enabled` reads. A file that cannot be read or parsed could be
/// the one narrowing authority, so it fails closed rather than falling through.
pub fn resolve(cwd: &Path) -> Policy {
    let mut mode = None;
    let mut allow = None;
    for path in config_candidates(cwd) {
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return fail_closed(path.display().to_string()),
        };
        let Some(table) = parse_config(&content) else {
            return fail_closed(path.display().to_string());
        };
        let Some(block) = table.get("autonomy").and_then(toml::Value::as_table) else {
            continue;
        };
        if mode.is_none() {
            if let Some(leaf) = block.get("mode") {
                mode = Some((parse_mode(leaf), path.display().to_string()));
            }
        }
        if allow.is_none() {
            allow = block.get("allow").map(parse_allow);
        }
    }
    let (mode, source) = mode.unwrap_or((Mode::Balanced, "default".to_string()));
    Policy {
        mode,
        allow: allow.unwrap_or_default(),
        source,
    }
}

fn parse_mode(leaf: &toml::Value) -> Mode {
    match leaf.as_str().map(str::trim) {
        Some("balanced") => Mode::Balanced,
        Some("yolo") => Mode::Yolo,
        _ => Mode::Ask,
    }
}

fn parse_allow(leaf: &toml::Value) -> Vec<Action> {
    leaf.as_array()
        .into_iter()
        .flatten()
        .filter_map(|word| match word.as_str().map(str::trim) {
            Some("restart") => Some(Action::Restart),
            Some("merge") => Some(Action::Merge),
            Some("law") => Some(Action::Law),
            _ => None,
        })
        .collect()
}

/// May an unattended restart run here? The recovery and master switches stay
/// with each caller: they veto in every mode.
pub fn restart_allowed(cwd: &Path) -> bool {
    resolve(cwd).disposition(Action::Restart) == Disposition::Auto
}

/// Does a session's law write here need the user's words?
pub fn law_needs_quote(cwd: &Path) -> bool {
    resolve(cwd).disposition(Action::Law) == Disposition::Ask
}

/// Apply the merge disposition over one standing `auto_merge` value. `None`
/// means no config set the value, which yolo reads as yes and an explicit
/// `Some(false)` stays a hold in every mode.
pub fn merge_value(disposition: Disposition, configured: Option<bool>) -> bool {
    match disposition {
        Disposition::Ask => false,
        Disposition::ConfigDecides => configured == Some(true),
        Disposition::Auto => configured != Some(false),
    }
}

/// The `policy` block of `fno agents admit autonomy status --json`.
pub fn status_json(cwd: &Path) -> Value {
    let policy = resolve(cwd);
    let word = |action| policy.disposition(action).word();
    json!({
        "mode": policy.mode.word(),
        "allow": policy.allow.iter().map(|a| a.word()).collect::<Vec<_>>(),
        "source": policy.source,
        "restart": word(Action::Restart),
        "merge": word(Action::Merge),
        "law": word(Action::Law),
        "merge_enabled": crate::agents_config::auto_merge_enabled(cwd),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: Mode, allow: &[Action]) -> Policy {
        Policy {
            mode,
            allow: allow.to_vec(),
            source: "test".to_string(),
        }
    }

    #[test]
    fn autonomy_policy_matrix_answers_every_mode_and_class() {
        use Action::{Law, Merge, Restart};
        use Disposition::{Ask, Auto, ConfigDecides};
        let all = [Restart, Merge, Law];
        let cases: [(Policy, [Disposition; 3]); 4] = [
            (policy(Mode::Ask, &[]), [Ask, Ask, Ask]),
            (policy(Mode::Ask, &all), [Auto, ConfigDecides, Auto]),
            (policy(Mode::Balanced, &[]), [Auto, ConfigDecides, Ask]),
            (policy(Mode::Yolo, &[]), [Auto, Auto, Auto]),
        ];
        for (policy, want) in cases {
            for (action, want) in all.into_iter().zip(want) {
                assert_eq!(policy.disposition(action), want, "{policy:?} {action:?}");
            }
        }
        // An allow list names one class and widens no other.
        let one = policy(Mode::Ask, &[Restart]);
        assert_eq!(one.disposition(Restart), Auto);
        assert_eq!(one.disposition(Merge), Ask);
        // An allow list is for ask: it never narrows balanced.
        assert_eq!(policy(Mode::Balanced, &[Merge]).disposition(Restart), Auto);
    }

    #[test]
    fn autonomy_policy_merge_keeps_an_explicit_no_in_every_mode() {
        for (disposition, unset, yes, no) in [
            (Disposition::Ask, false, false, false),
            (Disposition::ConfigDecides, false, true, false),
            (Disposition::Auto, true, true, false),
        ] {
            assert_eq!(merge_value(disposition, None), unset, "{disposition:?}");
            assert_eq!(merge_value(disposition, Some(true)), yes, "{disposition:?}");
            assert_eq!(merge_value(disposition, Some(false)), no, "{disposition:?}");
        }
    }

    #[test]
    fn autonomy_policy_a_bad_value_never_widens() {
        for bad in ["\"nonsense\"", "true", "3"] {
            let table = parse_config(&format!("mode = {bad}")).unwrap();
            assert_eq!(parse_mode(table.get("mode").unwrap()), Mode::Ask, "{bad}");
        }
        let table = parse_config("allow = [\"merge\", \"everything\", 3]").unwrap();
        assert_eq!(
            parse_allow(table.get("allow").unwrap()),
            vec![Action::Merge]
        );
        let table = parse_config("allow = \"merge\"").unwrap();
        assert!(parse_allow(table.get("allow").unwrap()).is_empty());
    }
}
