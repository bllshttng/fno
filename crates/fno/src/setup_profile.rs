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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// What a config key is, decided once, here.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Secret,
    Local,
    Portable,
}

/// Keys that name this exact machine, by full dotted key.
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

/// One exported profile: the TOML text, plus what stayed behind, counted
/// and named for the caller's summary line.
pub(crate) struct Exported {
    pub(crate) text: String,
    pub(crate) kept_local: Vec<String>,
    pub(crate) secrets: usize,
}

/// The export half: flatten the config, split every key by class, render
/// the portable half as a profile. Keys print sorted (BTreeMap order);
/// secrets count by name only, machine-local keys count but never print -
/// the text must be safe to paste into an issue.
pub(crate) fn export(flat: &BTreeMap<String, toml::Value>) -> Exported {
    let mut portable: Vec<(String, String)> = Vec::new();
    let mut kept_local: Vec<String> = Vec::new();
    let mut secret_count = 0usize;
    for (key, value) in flat {
        match classify(key, &crate::setup_run::toml_as_json(value)) {
            Class::Secret => secret_count += 1,
            Class::Local => kept_local.push(key.clone()),
            Class::Portable => portable.push((key.clone(), toml_inline(value))),
        }
    }
    let mut text = format!(
        "# fno profile: {} portable keys, {} machine-local, {} secret\n",
        portable.len(),
        kept_local.len(),
        secret_count
    );
    text.push_str("# join with: fno config setup run --yes --only join --join <this file>\n\n");
    text.push_str("[config]\n");
    for (key, value) in &portable {
        text.push_str(&format!("{key} = {value}\n"));
    }
    if portable.is_empty() {
        text.push_str("# (nothing portable)\n");
    }
    text.push_str("\n[profile]\nschema = 1\n");
    text.push_str(&format!(
        "exported_at = {}\n",
        quote_toml_string(&crate::setup_run::utc_now())
    ));
    text.push_str(&format!("host = {}\n", quote_toml_string(&host())));
    Exported {
        text,
        kept_local,
        secrets: secret_count,
    }
}

/// A loaded profile: its raw text (saved beside the backups on a join) and
/// its config flattened to dotted keys.
pub(crate) struct Profile {
    pub(crate) text: String,
    pub(crate) config: BTreeMap<String, toml::Value>,
    pub(crate) exported_at: String,
    pub(crate) host: String,
}

/// Parse and validate a profile before anything prints or writes: the
/// `[profile]` header and `schema = 1` gate every join. A `config` table
/// is flattened so the diff sees `auto_merge.enabled`, not nested tables.
pub(crate) fn parse_profile(text: &str) -> Result<Profile, String> {
    let doc: toml::Value =
        toml::from_str(text).map_err(|e| format!("this does not read as TOML: {e}"))?;
    let profile = doc
        .get("profile")
        .ok_or_else(|| "no [profile] header; this does not read as an fno profile".to_string())?;
    let schema = profile
        .get("schema")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| {
            "no integer profile.schema; this does not read as an fno profile".to_string()
        })?;
    if schema != 1 {
        return Err(format!(
            "unknown profile schema {schema}; this fno joins schema 1"
        ));
    }
    let mut config = BTreeMap::new();
    if let Some(table) = doc.get("config").and_then(toml::Value::as_table) {
        flatten("", table, &mut config);
    }
    let string_of = |k: &str, default: &str| {
        profile
            .get(k)
            .and_then(toml::Value::as_str)
            .unwrap_or(default)
            .to_string()
    };
    Ok(Profile {
        text: text.to_string(),
        config,
        exported_at: string_of("exported_at", "unknown"),
        host: string_of("host", "unknown"),
    })
}

/// Recursively flatten a TOML table into dotted keys.
fn flatten(prefix: &str, table: &toml::value::Table, out: &mut BTreeMap<String, toml::Value>) {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(t) => flatten(&key, t, out),
            other => {
                out.insert(key, other.clone());
            }
        }
    }
}

/// Load a profile from a path, `-` meaning stdin.
pub(crate) fn load_profile(path: &str) -> Result<Profile, String> {
    let text = if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("-: {e}"))?;
        buf
    } else {
        std::fs::read_to_string(Path::new(path)).map_err(|e| format!("{path}: {e}"))?
    };
    parse_profile(&text)
}

/// One key's travel story, in the diff shown before a join writes.
pub(crate) struct DiffRow {
    pub(crate) kind: &'static str,
    pub(crate) key: String,
    pub(crate) old: Option<String>,
    pub(crate) new: Option<String>,
    /// The portable value to write, set only on an `add` or `change` row.
    pub(crate) apply: Option<toml::Value>,
}

impl DiffRow {
    /// Whether the join writes this row.
    pub(crate) fn applies(&self) -> bool {
        self.kind == "add" || self.kind == "change"
    }
}

/// The profile against this machine: adds, changes, sames, kept-locals,
/// plus the secret count. BTreeMap order keeps the rows sorted by key.
pub(crate) fn diff(
    profile: &Profile,
    here: &BTreeMap<String, toml::Value>,
) -> (Vec<DiffRow>, usize) {
    let mut rows = Vec::new();
    let mut secrets = 0usize;
    for (key, value) in &profile.config {
        match classify(key, &crate::setup_run::toml_as_json(value)) {
            Class::Secret => secrets += 1,
            Class::Local => rows.push(DiffRow {
                kind: "kept_local",
                key: key.clone(),
                old: None,
                new: None,
                apply: None,
            }),
            Class::Portable => {
                let json = crate::setup_run::toml_as_json(value);
                let new = toml_inline(value);
                match here.get(key) {
                    None => rows.push(DiffRow {
                        kind: "add",
                        key: key.clone(),
                        old: None,
                        new: Some(new),
                        apply: Some(value.clone()),
                    }),
                    Some(hv) if crate::setup_run::toml_as_json(hv) == json => rows.push(DiffRow {
                        kind: "same",
                        key: key.clone(),
                        old: None,
                        new: None,
                        apply: None,
                    }),
                    Some(hv) => rows.push(DiffRow {
                        kind: "change",
                        key: key.clone(),
                        old: Some(toml_inline(hv)),
                        new: Some(new),
                        apply: Some(value.clone()),
                    }),
                }
            }
        }
    }
    (rows, secrets)
}

/// The human diff banner printed before a join (and by --export as a
/// preview): one line per key, secrets counted by name only.
pub(crate) fn format_diff(
    rows: &[DiffRow],
    secrets: usize,
    host: &str,
    exported_at: &str,
) -> String {
    let mut text = format!("join: profile from {host}, exported {exported_at}\n");
    if rows.is_empty() {
        text.push_str("  (the profile adds nothing: every key is present or machine-local)\n");
    }
    for r in rows {
        match r.kind {
            "add" => text.push_str(&format!(
                "  add     {} = {}\n",
                r.key,
                r.new.as_deref().unwrap_or("")
            )),
            "change" => text.push_str(&format!(
                "  change  {} = {} (was {})\n",
                r.key,
                r.new.as_deref().unwrap_or(""),
                r.old.as_deref().unwrap_or("")
            )),
            "same" => text.push_str(&format!("  same    {}\n", r.key)),
            _ => text.push_str(&format!("  kept    {} (machine-local)\n", r.key)),
        }
    }
    if secrets > 0 {
        text.push_str(&format!(
            "  ({secrets} secret key(s) never travel and are not shown)\n"
        ));
    }
    text
}

/// Render a TOML value as the right-hand side of a `key = value` line:
/// valid TOML for every variant, so what the diff shows is what joins.
pub(crate) fn toml_inline(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => quote_toml_string(s),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                format!("{f:.1}")
            } else {
                f.to_string()
            }
        }
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(toml_inline).collect();
            format!("[{}]", parts.join(", "))
        }
        toml::Value::Table(t) => {
            let parts: Vec<String> = t
                .iter()
                .map(|(k, v)| format!("{} = {}", inline_key(k), toml_inline(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

/// A bare dotted key when every segment is alnum, `_` or `-`; any other
/// segment quoted. Dotted keys stay bare so the TOML reads as written.
pub(crate) fn inline_key(k: &str) -> String {
    k.split('.')
        .map(|seg| {
            if !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                seg.to_string()
            } else {
                quote_toml_string(seg)
            }
        })
        .collect::<Vec<String>>()
        .join(".")
}

/// A TOML basic string: the two specials escaped, the common whitespace
/// escapes, and `\u00XX` for control chars.
pub(crate) fn quote_toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// This machine's name for the profile header: $HOST when set (agent
/// sessions name themselves), else `hostname`, else unknown.
pub(crate) fn host() -> String {
    if let Ok(h) = std::env::var("HOST") {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    crate::setup_detect::bounded_output(&["hostname"])
        .ok()
        .map(|o| o.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// The sidecar folder every join artifact lives in, beside the setup
/// markers: the state root grows no new top-level row (the freeze gate).
pub(crate) fn backup_dir() -> PathBuf {
    crate::model_catalog::state_dir()
        .join("sidecar")
        .join("setup-backup")
}

/// The in-progress flag a join stamps before its first write, so a run
/// that dies mid-join is healed on the next start.
pub(crate) fn in_progress_flag() -> PathBuf {
    backup_dir().join("in-progress")
}

/// Where a joined profile is saved, so the operator can diff or re-export.
pub(crate) fn profile_path() -> PathBuf {
    crate::model_catalog::state_dir()
        .join("sidecar")
        .join("setup-profile.toml")
}

/// Copy the global config into the backup dir. `Ok(None)` on a fresh
/// machine: there is nothing to restore, and the join proceeds.
pub(crate) fn backup(global: &Path) -> Result<Option<PathBuf>, String> {
    if !global.exists() {
        return Ok(None);
    }
    let dir = backup_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = dir.join(format!("config.toml.{}", crate::setup_run::utc_now()));
    std::fs::copy(global, &target).map_err(|e| format!("{}: {e}", global.display()))?;
    Ok(Some(target))
}

/// The newest `config.toml.*` backup, for the heal. Name order is stamp
/// order: the backup file name carries the ISO UTC stamp.
pub(crate) fn newest_backup() -> Option<PathBuf> {
    let mut best: Option<(std::ffi::OsString, PathBuf)> = None;
    for e in std::fs::read_dir(backup_dir()).ok()?.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().starts_with("config.toml.") {
            continue;
        }
        if best.as_ref().map(|(n, _)| &name > n).unwrap_or(true) {
            best = Some((name, e.path()));
        }
    }
    best.map(|(_, p)| p)
}

/// Copy a backup back over the global config. `Err` when no backup exists:
/// a caller must not invent a restore that never had a source.
pub(crate) fn restore(global: &Path, backup: &Option<PathBuf>) -> Result<(), String> {
    let Some(b) = backup else {
        return Err("no backup exists for this join".into());
    };
    std::fs::copy(b, global)
        .map_err(|e| format!("{} -> {}: {e}", b.display(), global.display()))?;
    Ok(())
}

/// The join-crash heal, run before any global-config read on every start:
/// with no in-progress flag there is nothing to do (`None`). A flag with
/// no backup, or a failed restore, keeps the flag and reports; a restored
/// config removes the flag. The boolean is whether the heal succeeded.
pub(crate) fn heal() -> Option<(bool, String)> {
    if !in_progress_flag().exists() {
        return None;
    }
    let Some(b) = newest_backup() else {
        return Some((
            false,
            format!(
                "a join was interrupted and no backup exists in {}; the flag is kept",
                backup_dir().display()
            ),
        ));
    };
    match restore(
        &crate::config_defaults::global_config_path(),
        &Some(b.clone()),
    ) {
        Err(e) => Some((
            false,
            format!(
                "a join was interrupted and the restore from {} failed: {e}; the flag is kept",
                b.display()
            ),
        )),
        Ok(()) => {
            let _ = std::fs::remove_file(in_progress_flag());
            Some((
                true,
                format!(
                    "restored the global config from {} after a join died mid-write",
                    b.file_name().unwrap_or_default().to_string_lossy()
                ),
            ))
        }
    }
}

/// Stamp the in-progress flag before the first config write.
pub(crate) fn mark_in_progress() -> std::io::Result<()> {
    std::fs::create_dir_all(backup_dir())?;
    std::fs::write(in_progress_flag(), crate::setup_run::utc_now())
}

/// Save the joined profile beside the backups. Best-effort: an error here
/// never fails the join.
pub(crate) fn save_profile(text: &str) {
    let path = profile_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

/// This machine's flattened config, for the diff. An absent file is an
/// empty map; a broken one is an error the caller refuses on.
pub(crate) fn here_config() -> Result<BTreeMap<String, toml::Value>, String> {
    match crate::config_defaults::read_flat(&crate::config_defaults::global_config_path()) {
        Err(e) => Err(e),
        Ok(None) => Ok(BTreeMap::new()),
        Ok(Some(map)) => Ok(map),
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

    /// The export splits the classes the way the join consumes them: the
    /// two ruled-portable keys travel, the machine-local key is named only
    /// on the struct, and neither the secret's name nor its value reaches
    /// the text. The text reads back as the same config, and the diff
    /// shapes add/change rows against a machine that differs.
    #[test]
    fn the_export_splits_classes_and_the_diff_shapes_rows() {
        let mut flat = BTreeMap::new();
        flat.insert("auto_merge.enabled".to_string(), toml::Value::Boolean(true));
        flat.insert(
            "review.posture".to_string(),
            toml::Value::String("standard".into()),
        );
        flat.insert(
            "paths.worktrees_base".to_string(),
            toml::Value::String("/Users/bb16/.fno/worktrees".into()),
        );
        flat.insert(
            "store.remote_token".to_string(),
            toml::Value::String("t0k".into()),
        );
        let exported = export(&flat);
        assert!(exported.text.contains("auto_merge.enabled = true"));
        assert!(exported.text.contains("review.posture = \"standard\""));
        assert!(!exported.text.contains("worktrees_base"));
        assert!(!exported.text.contains("remote_token"));
        assert!(!exported.text.contains("t0k"));
        assert_eq!(
            exported.kept_local,
            vec!["paths.worktrees_base".to_string()]
        );
        assert_eq!(exported.secrets, 1);
        let profile = parse_profile(&exported.text).unwrap();
        assert_eq!(profile.config.len(), 2);
        let mut here = BTreeMap::new();
        here.insert(
            "review.posture".to_string(),
            toml::Value::String("strict".into()),
        );
        let (rows, secrets) = diff(&profile, &here);
        assert_eq!(secrets, 0);
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec!["add", "change"]);
        let banner = format_diff(&rows, secrets, &profile.host, &profile.exported_at);
        assert!(banner.contains("add     auto_merge.enabled = true"));
        assert!(banner.contains("change  review.posture = \"standard\" (was \"strict\")"));
        assert!(!banner.contains("token"));
    }

    /// The header gates the join: no `[profile]`, no join; a future schema
    /// names itself and the one this fno reads. A hand-written profile's
    /// secrets count without printing, and its machine-local keys report
    /// as kept.
    #[test]
    fn the_profile_header_gates_the_join() {
        let err = parse_profile("[config]\nreview.posture = \"standard\"\n").unwrap_err();
        assert!(err.contains("[profile]"));
        let err = parse_profile("[profile]\nschema = 9\n").unwrap_err();
        assert!(err.contains("9"));
        let text =
            "[config]\nauth.oauth_token = \"t0k\"\nagents.max_live = 3\n\n[profile]\nschema = 1\n";
        let profile = parse_profile(text).unwrap();
        let here = BTreeMap::new();
        let (rows, secrets) = diff(&profile, &here);
        assert_eq!(secrets, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "kept_local");
        assert_eq!(rows[0].key, "agents.max_live");
        let banner = format_diff(&rows, secrets, &profile.host, &profile.exported_at);
        assert!(!banner.contains("t0k"));
    }
}
