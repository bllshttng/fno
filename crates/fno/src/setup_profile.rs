//! The key classes every setup surface shares: `secret`, machine-local,
//! portable. A secret never travels and its name never prints in a
//! model-readable JSON; a machine-local value describes this machine and
//! stays on it; everything else is portable and may ride an export or a
//! join. The same table drives one redact function, and every JSON the
//! model can read passes through it: `--dry-run --json`, the join diff,
//! `--check --json`, the run report and the wizard report. No secret-class
//! key name or value appears in any of them.
//!
//! This file starts as the pure classification core - the class table and
//! `redact` - because the dry-run prints through `redact` from its first
//! commit. The export, diff, backup and join half builds on `classify`.

use serde_json::Value;

/// What a config key is, decided once, here.
#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum Class {
    Secret,
    Local,
    Portable,
}

/// Keys that name this exact machine, by full dotted key.
#[allow(dead_code)]
const LOCAL_EXACT: &[&str] = &[
    "state_dir",
    "plans_dir",
    "obsidian.vault",
    "research.output_dir",
    "lead.enabled",
    "agents.max_live",
    "agents.max_live_per_territory",
];

/// Key families that name this machine, by dotted prefix.
#[allow(dead_code)]
const LOCAL_PREFIX: &[&str] = &[
    "paths.",
    "work.",
    "user.",
    "build.",
    "accounts.",
    "agents.provider_limits.",
    "store.",
];

/// Whether a leaf key name is a secret by shape: the leaf (the last dotted
/// segment) carries token, secret or password, or ends `api_key`. An
/// `api_key_env` leaf names an env var, not a value, so the ends-with test
/// skips it on purpose.
fn secret_name(name: &str) -> bool {
    let leaf = name.rsplit('.').next().unwrap_or(name);
    let lower = leaf.to_lowercase();
    ["token", "secret", "password"]
        .iter()
        .any(|s| lower.contains(s))
        || lower.ends_with("api_key")
}

/// The one class decision. Secret by leaf shape wins; then the exact and
/// prefix tables; then any string value that starts `/` or `~` is a path on
/// this machine; everything else travels.
#[allow(dead_code)]
pub(crate) fn classify(key: &str, value: &Value) -> Class {
    if secret_name(key) {
        return Class::Secret;
    }
    if LOCAL_EXACT.contains(&key) || LOCAL_PREFIX.iter().any(|p| key.starts_with(p)) {
        return Class::Local;
    }
    if let Value::String(s) = value {
        if s.starts_with('/') || s.starts_with('~') {
            return Class::Local;
        }
    }
    Class::Portable
}

/// Walk a JSON tree and drop every secret-class key. Dropping, not
/// blanking: no secret-class key name may appear in a model-readable
/// document, so the key goes away with its value. Objects copy themselves
/// key by key, arrays map over themselves, leaves clone.
pub(crate) fn redact(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !secret_name(k.as_str()))
                .map(|(k, val)| (k.clone(), redact(val)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The one class-table test: every rule the file carries, as rows,
    /// plus the redact walk that drops secret keys wherever they sit.
    #[test]
    fn the_class_table_sorts_keys_and_redact_drops_secrets() {
        let cases: &[(&str, Value, Class)] = &[
            // Secret by leaf shape; api_key_env is not a secret.
            (
                "providers.anthropic.api_key",
                json!("sk-live"),
                Class::Secret,
            ),
            ("auth.oauth_token", json!("t0k"), Class::Secret),
            ("smtp.password", json!("hunter2"), Class::Secret),
            (
                "providers.openai.api_key_env",
                json!("OPENAI_API_KEY"),
                Class::Portable,
            ),
            // Machine-local by exact key and by family prefix.
            ("state_dir", json!("/Users/x/.fno"), Class::Local),
            ("agents.max_live", json!(3), Class::Local),
            ("obsidian.vault", json!("~/c3po"), Class::Local),
            ("accounts.primary", json!("claude"), Class::Local),
            ("agents.provider_limits.zai", json!(5), Class::Local),
            ("store.kind", json!("sqlite"), Class::Local),
            // Portable: the review and auto-merge families travel by ruling.
            ("review.github_apps", json!(["bot"]), Class::Portable),
            ("auto_merge.enabled", json!(true), Class::Portable),
            // Any string starting / or ~ is a path on this machine.
            ("custom.thing", json!("/opt/where"), Class::Local),
            ("custom.thing", json!("~/rel"), Class::Local),
            ("custom.thing", json!("plain"), Class::Portable),
        ];
        for (key, value, want) in cases {
            assert_eq!(classify(key, &value), *want, "key {key}");
        }
        let doc = json!({
            "providers": {"anthropic": {"api_key": "sk-live", "model": "opus"}},
            "rows": [{"oauth_token": "t0k"}, {"key": "plain"}],
            "state_dir": "/Users/x/.fno",
        });
        let out = redact(&doc);
        assert!(out["providers"]["anthropic"].get("api_key").is_none());
        assert_eq!(out["providers"]["anthropic"]["model"], "opus");
        assert!(out["rows"][0].get("oauth_token").is_none());
        assert_eq!(out["rows"][1]["key"], "plain");
        assert_eq!(out["state_dir"], "/Users/x/.fno");
    }
}
