//! The user's `keybindings.json`: the one place mux key choices live.
//!
//! The file sits beside the global `config.toml` and is FLAT: every top-level
//! key is a stable action id from the shipped table (`keys::default_bindings`)
//! mapped to one key string, plus the reserved `"preset"` key. No groups, no
//! nesting; context lives in the action id. The built-in table and the named
//! presets stay compiled into the binary, so an upgrade moves them and the
//! file only ever holds what the user chose.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::keys::{self, KeymapWarning};

pub const FILE_NAME: &str = "keybindings.json";
const PRESET_KEY: &str = "preset";

/// Where the file lives: the sibling of the global `config.toml`.
pub fn path() -> Option<PathBuf> {
    crate::digest_overlay::global_config_toml().map(|p| p.with_file_name(FILE_NAME))
}

/// What one file said, plus everything it could not use.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    pub preset: Option<String>,
    pub rows: Vec<(String, String)>,
    pub warnings: Vec<KeymapWarning>,
}

/// Read a file's text as keybindings. Unknown action ids are left for
/// [`keys::resolve_keymap`], which warns and skips them; a value that is not a
/// key string (a nested object, an array, a number) is warned and skipped
/// here, because the resolver only ever sees strings.
pub fn parse(path: &Path, content: &str) -> Loaded {
    let mut out = Loaded::default();
    let warn = |out: &mut Loaded, msg: String| out.warnings.push(KeymapWarning(msg));
    let value: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            warn(
                &mut out,
                format!("keys on defaults: bad JSON in {} ({e})", path.display()),
            );
            return out;
        }
    };
    let Some(object) = value.as_object() else {
        warn(
            &mut out,
            format!(
                "keys on defaults: {} must be one flat object of action: key",
                path.display()
            ),
        );
        return out;
    };
    for (id, v) in object {
        match v.as_str() {
            Some(s) if id == PRESET_KEY => out.preset = Some(s.to_string()),
            Some(s) => out.rows.push((id.clone(), s.to_string())),
            None => warn(
                &mut out,
                format!("keybindings.{id}: needs a quoted key, not {v}; one level only"),
            ),
        }
    }
    out
}

/// `Ok(None)` is an absent file; every other read failure is a file the user
/// has and the mux cannot use, so it becomes a warning.
pub fn load(path: &Path) -> Result<Option<Loaded>, KeymapWarning> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(parse(path, &content))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(KeymapWarning(format!(
            "keys on defaults: cannot read {} ({e})",
            path.display()
        ))),
    }
}

/// The key string a preset (or the built-in table) gives each bound action,
/// in table order. Unbound rows (no default chord) are left out.
fn resolved_rows(preset: &str) -> Vec<(&'static str, String)> {
    let preset_rows = keys::preset_rows(preset).unwrap_or(&[]);
    keys::default_bindings()
        .iter()
        .filter_map(|kb| {
            let spec = preset_rows
                .iter()
                .find(|(a, _)| *a == kb.action)
                .map(|(_, k)| k.to_string())
                .or_else(|| (kb.key != 0).then(|| keys::key_disp(kb.key)))?;
            Some((kb.action, spec))
        })
        .collect()
}

/// Drop rows that restate the key the preset (or the built-in table) already
/// gives. A spelled-out starter would otherwise reach the resolver as dozens of
/// no-op rebinds and fill `Keymap::rebinds` with the whole table.
pub fn drop_noops(preset: Option<&str>, rows: Vec<(String, String)>) -> Vec<(String, String)> {
    let base: Vec<(&'static str, String)> = resolved_rows(preset.unwrap_or("tmux"));
    rows.into_iter()
        .filter(|(action, spec)| {
            let action = action.trim().to_ascii_lowercase();
            let same = base.iter().find(|(a, _)| *a == action).and_then(|(_, k)| {
                Some(keys::parse_key(k)? == keys::parse_key(spec)?)
            });
            same != Some(true)
        })
        .collect()
}

/// A starter file: the chosen preset spelled out, one row per bound action,
/// for the user to edit down or change in place.
pub fn starter(preset: &str) -> String {
    let mut table = serde_json::Map::new();
    table.insert(PRESET_KEY.into(), preset.into());
    for (action, spec) in resolved_rows(preset) {
        table.insert(action.into(), spec.into());
    }
    let mut text = serde_json::to_string_pretty(&table).unwrap_or_default();
    text.push('\n');
    text
}

/// A sparse file: the preset plus exactly the rows given. Used when moving
/// the retired `config.toml` keys here and when one row changes.
pub fn sparse(preset: Option<&str>, rows: &[(String, String)]) -> String {
    let mut table = serde_json::Map::new();
    if let Some(p) = preset {
        table.insert(PRESET_KEY.into(), p.into());
    }
    for (action, spec) in rows {
        table.insert(action.clone(), spec.clone().into());
    }
    let mut text = serde_json::to_string_pretty(&table).unwrap_or_default();
    text.push('\n');
    text
}

/// Write [`starter`] where the file belongs unless a file is already there.
/// Returns the path either way, for the caller to open.
pub fn ensure_starter(preset: &str) -> std::io::Result<PathBuf> {
    let path = path().ok_or_else(|| std::io::Error::other("no HOME to place keybindings.json"))?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, starter(preset))?;
    }
    Ok(path)
}

/// The generated action reference: every action id with its shipped key, the
/// key each preset gives it, and the label the which-key modal shows. Built
/// from the same tables the dispatcher runs, so `docs/mux-key-reference.md`
/// cannot drift from them (a test compares the two).
pub fn reference_markdown() -> String {
    let show = |key: u8| {
        if key == 0 {
            "unbound".to_string()
        } else {
            format!("`{}`", keys::key_disp(key))
        }
    };
    let preset_key = |preset: &str, kb: &keys::KeyBinding| {
        keys::preset_rows(preset)
            .and_then(|rows| rows.iter().find(|(a, _)| *a == kb.action))
            .and_then(|(_, spec)| keys::parse_key(spec))
            .map_or_else(|| show(kb.key), show)
    };
    let mut out = String::from(
        "# Mux key reference\n\n\
         Generated from `default_bindings` and `preset_rows` in `crates/fno/src/keys.rs`. \
         Do not edit by hand: run `FNO_REGEN_KEY_DOCS=1 cargo test -p fno key_reference` to rewrite it.\n\n\
         Each row is one action id. Put the id in `~/.fno/keybindings.json` with a key string to rebind it. \
         The file is one flat object, and `\"preset\"` is its one reserved key. \
         The keys below come after the prefix.\n\n\
         | Action id | Built-in | tmux | zellij | Meaning |\n|---|---|---|---|---|\n",
    );
    for kb in keys::default_bindings() {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            kb.action,
            show(kb.key),
            preset_key("tmux", &kb),
            preset_key("zellij", &kb),
            kb.label,
        ));
    }
    out
}

/// Where a live key came from, for the modal's tag column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    BuiltIn,
    Preset,
    Yours,
}

impl Origin {
    pub fn tag(self) -> &'static str {
        match self {
            Origin::BuiltIn => "built-in",
            Origin::Preset => "preset",
            Origin::Yours => "yours",
        }
    }
}

static ACTIVE_PRESET: RwLock<Option<String>> = RwLock::new(None);

/// Record the preset the installed keymap was resolved with.
pub fn set_active_preset(preset: Option<&str>) {
    if let Ok(mut slot) = ACTIVE_PRESET.write() {
        *slot = preset
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty());
    }
}

fn active_preset() -> Option<String> {
    ACTIVE_PRESET.read().ok().and_then(|p| p.clone())
}

/// Classify a live binding. A key that differs from what the preset (or the
/// built-in table) would give is the user's, whether or not the file spells
/// out the rows around it.
pub fn origin(action: &str, live_key: u8) -> Origin {
    origin_under(active_preset().as_deref(), action, live_key)
}

fn origin_under(preset: Option<&str>, action: &str, live_key: u8) -> Origin {
    let preset_key = preset
        .and_then(keys::preset_rows)
        .and_then(|rows| rows.iter().find(|(a, _)| *a == action))
        .and_then(|(_, spec)| keys::parse_key(spec));
    let default_key = keys::default_bindings()
        .iter()
        .find(|kb| kb.action == action)
        .map(|kb| kb.key);
    match (preset_key, default_key) {
        (Some(p), _) if p == live_key => Origin::Preset,
        (None, Some(d)) if d == live_key => Origin::BuiltIn,
        _ => Origin::Yours,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PathBuf {
        PathBuf::from("/x/keybindings.json")
    }

    #[test]
    fn flat_object_splits_preset_from_rows() {
        let l = parse(&p(), r#"{"preset":"zellij","detach":"Q","find":"F"}"#);
        assert_eq!(l.preset.as_deref(), Some("zellij"));
        assert_eq!(
            l.rows,
            vec![("detach".into(), "Q".into()), ("find".into(), "F".into())]
        );
        assert!(l.warnings.is_empty());
    }

    #[test]
    fn nesting_numbers_and_bad_json_warn_instead_of_failing() {
        let l = parse(
            &p(),
            r#"{"panes":{"split-right":"r"},"detach":3,"find":"F"}"#,
        );
        assert_eq!(l.rows, vec![("find".to_string(), "F".to_string())]);
        assert_eq!(l.warnings.len(), 2);
        assert!(parse(&p(), "{nope").warnings[0].0.contains("bad JSON"));
        assert!(parse(&p(), "[1]").warnings[0].0.contains("flat object"));
    }

    #[test]
    fn starter_spells_out_the_preset_and_round_trips() {
        let l = parse(&p(), &starter("zellij"));
        assert_eq!(l.preset.as_deref(), Some("zellij"));
        let get = |a: &str| l.rows.iter().find(|(k, _)| k == a).map(|(_, v)| v.as_str());
        assert_eq!(get("split-right"), Some("r"));
        assert_eq!(get("detach"), Some("Q"));
        assert_eq!(get("split-up"), None, "unbound rows stay out");
        // Fed back through the resolver, a spelled-out starter changes nothing.
        let rows = drop_noops(l.preset.as_deref(), l.rows);
        assert!(rows.is_empty(), "a spelled-out starter restates only defaults: {rows:?}");
        let (entries, w) = keys::with_preset(l.preset.as_deref(), rows);
        let (map, warnings) = keys::resolve_keymap(None, &entries);
        assert!(w.is_empty() && warnings.is_empty(), "{w:?} {warnings:?}");
        let (zellij, _) =
            keys::resolve_keymap(None, &keys::with_preset(Some("zellij"), Vec::new()).0);
        assert_eq!(map.rebinds.len(), zellij.rebinds.len());
    }

    #[test]
    fn origin_tags_builtin_preset_and_yours() {
        assert_eq!(origin_under(None, "detach", b'd'), Origin::BuiltIn);
        assert_eq!(origin_under(Some("zellij"), "detach", b'Q'), Origin::Preset);
        assert_eq!(origin_under(Some("zellij"), "find", b'f'), Origin::BuiltIn);
        assert_eq!(origin_under(Some("zellij"), "detach", b'z'), Origin::Yours);
        assert_eq!(origin_under(None, "detach", b'Q'), Origin::Yours);
    }

    #[test]
    fn key_reference_matches_the_checked_in_page() {
        let page = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/mux-key-reference.md");
        let want = reference_markdown();
        if std::env::var_os("FNO_REGEN_KEY_DOCS").is_some() {
            std::fs::write(&page, &want).unwrap();
        }
        let have = std::fs::read_to_string(&page).unwrap_or_default();
        assert_eq!(
            have, want,
            "docs/mux-key-reference.md is stale: run `FNO_REGEN_KEY_DOCS=1 cargo test -p fno key_reference`"
        );
    }
}
