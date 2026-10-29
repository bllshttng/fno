//! The two install profiles of the fno plugin: full, and the pack.
//!
//! `pack-profiles.json` at the plugin root lists the mux paths: the skills
//! and hook scripts that need the daemon, mail or panes. A pack stage drops
//! them, and drops every hook entry that runs a mux script, so a harness with
//! no mux never calls one. `scripts/ci/check-pack-profiles.sh` keeps the list
//! whole.

use serde_json::Value;
use std::path::Path;

pub const MANIFEST: &str = "pack-profiles.json";

/// The hook files a pack stage rewrites.
pub const HOOK_FILES: [&str; 3] = [
    "hooks/hooks.json",
    "hooks/codex-hooks.json",
    "hooks/context-hooks.json",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Full,
    Pack,
}

impl Profile {
    /// The stage dir under plugin-stage. Two dirs, so one profile never
    /// replaces the other's tree under a live session.
    pub fn stage_dir(self) -> &'static str {
        match self {
            Profile::Full => "fno",
            Profile::Pack => "fno-pack",
        }
    }
}

pub struct MuxPaths(Vec<String>);

impl MuxPaths {
    /// Read the manifest under `root`. A listed path the tree does not hold
    /// refuses, so a renamed mux file can never ship in a pack by accident.
    pub fn load(root: &Path) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(root.join(MANIFEST)).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let paths: Vec<String> = v
            .get("mux")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{MANIFEST}: no mux list"))?
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect();
        if let Some(gone) = paths.iter().find(|p| !root.join(p).exists()) {
            return Err(format!("{MANIFEST} names {gone}, which does not exist"));
        }
        Ok(Self(paths))
    }

    /// True when `rel` is a mux path or sits under a mux dir.
    pub fn covers(&self, rel: &str) -> bool {
        self.0.iter().any(|p| {
            rel == p
                || rel
                    .strip_prefix(p.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }

    fn runs_mux(&self, command: &str) -> bool {
        self.0
            .iter()
            .filter(|p| p.starts_with("hooks/") || p.starts_with("scripts/"))
            .any(|p| command.contains(&format!("/{p}")))
    }

    /// One of HOOK_FILES with every mux entry dropped. A matcher group left
    /// with no hooks goes, and so does an event left with no groups.
    pub fn filter_hooks(&self, text: &str) -> Result<String, String> {
        let mut v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mux_command = |entry: &Value| {
            entry
                .get("command")
                .and_then(Value::as_str)
                .or_else(|| {
                    entry
                        .get("argv")
                        .and_then(|a| a.get(0))
                        .and_then(Value::as_str)
                })
                .is_some_and(|c| self.runs_mux(c))
        };
        if let Some(events) = v.get_mut("hooks").and_then(Value::as_object_mut) {
            for groups in events.values_mut().filter_map(Value::as_array_mut) {
                groups.retain_mut(|g| match g.get_mut("hooks").and_then(Value::as_array_mut) {
                    Some(hooks) => {
                        hooks.retain(|h| !mux_command(h));
                        !hooks.is_empty()
                    }
                    None => true,
                });
            }
            events.retain(|_, groups| groups.as_array().map_or(true, |l| !l.is_empty()));
        }
        if let Some(groups) = v.get_mut("groups").and_then(Value::as_object_mut) {
            for group in groups.values_mut() {
                if let Some(producers) = group.get_mut("producers").and_then(Value::as_array_mut) {
                    producers.retain(|p| !mux_command(p));
                }
            }
        }
        serde_json::to_string_pretty(&v)
            .map(|s| s + "\n")
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_drops_mux_entries_and_the_groups_they_empty() {
        let root = std::env::temp_dir().join(format!("pack-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::create_dir_all(root.join("skills/mail")).unwrap();
        std::fs::write(root.join("hooks/mail.sh"), "").unwrap();
        std::fs::write(
            root.join(MANIFEST),
            r#"{"mux":["hooks/mail.sh","skills/mail"]}"#,
        )
        .unwrap();
        let mux = MuxPaths::load(&root).unwrap();
        assert!(mux.covers("skills/mail/SKILL.md"));
        assert!(!mux.covers("skills/mailbox/SKILL.md"));
        let hooks = r#"{"hooks":{"Stop":[{"hooks":[{"command":"\"${R}/hooks/mail.sh\""}]}],
            "PreToolUse":[{"hooks":[{"command":"${R}/hooks/mail.sh"},{"command":"${R}/hooks/gate.sh"}]}]}}"#;
        let out: Value = serde_json::from_str(&mux.filter_hooks(hooks).unwrap()).unwrap();
        assert_eq!(
            out,
            serde_json::json!({"hooks":{"PreToolUse":[{"hooks":[{"command":"${R}/hooks/gate.sh"}]}]}})
        );
        let context = r#"{"groups":{"s":{"producers":[{"argv":["${R}/hooks/mail.sh"]},{"argv":["${R}/hooks/v.sh"]}]}}}"#;
        let out: Value = serde_json::from_str(&mux.filter_hooks(context).unwrap()).unwrap();
        assert_eq!(out["groups"]["s"]["producers"].as_array().unwrap().len(), 1);
    }
}
