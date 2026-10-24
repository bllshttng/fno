//! The write side of the accounts config: `loader.save_providers` and
//! `atomic_mutate_settings`. The file is edited in place with toml_edit, so
//! comments and keys outside the accounts block survive a write; the Python
//! writer re-dumped the whole file and dropped them.
//!
//! Every write holds an exclusive flock on `<path>.lock`, the sidecar
//! `fno.state.io.atomic_write` locks, so a Rust write serializes with
//! `save_providers`, the one Python writer still live.

use crate::accounts::records::{self, Record};
use serde_json::Value as Json;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

/// Why a write did not land.
#[derive(Debug)]
pub enum WriteError {
    /// The file exists and does not parse or read: the Python
    /// `ProviderConfigError` text, so nothing is clobbered.
    Unreadable(String),
    /// The lock, temp file or rename failed.
    Io(std::io::Error),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Unreadable(msg) => f.write_str(msg),
            WriteError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for WriteError {
    fn from(e: std::io::Error) -> Self {
        WriteError::Io(e)
    }
}

/// `loader._settings_lock_path`.
pub fn lock_path(path: &Path) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(".lock");
    PathBuf::from(raw)
}

/// `atomic_mutate_settings`: read `path` (or its settings.yaml sibling) under
/// the exclusive lock, apply `edit`, then write a temp file and rename it over
/// `path`. A missing file starts empty; an unparseable one refuses. An `Err`
/// from `edit` writes nothing and comes back as the inner `Err`.
pub fn mutate<T, E>(
    path: &Path,
    edit: impl FnOnce(&mut DocumentMut) -> Result<T, E>,
) -> Result<Result<T, E>, WriteError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(lock_path(path))?;
    // SAFETY: flock on a descriptor this function owns; closing it on drop
    // releases the lock.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut doc = read_strict(path)?;
    let out = match edit(&mut doc) {
        Ok(out) => out,
        Err(e) => return Ok(Err(e)),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(doc.to_string().as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    drop(lock);
    Ok(Ok(out))
}

/// `loader._read_parsed_strict`, kept as an editable document.
fn read_strict(path: &Path) -> Result<DocumentMut, WriteError> {
    let Some(found) = records::read_candidates(path)
        .into_iter()
        .find(|c| c.is_file())
    else {
        return Ok(DocumentMut::new());
    };
    let text = std::fs::read_to_string(&found).map_err(|e| {
        WriteError::Unreadable(format!(
            "Cannot save: config file is not readable ({}): {e}",
            found.display()
        ))
    })?;
    let refuse = |e: &dyn std::fmt::Display| {
        WriteError::Unreadable(format!(
            "Cannot save: config file failed to parse ({}): {}",
            found.display(),
            e.to_string().trim_end()
        ))
    };
    if found.extension().is_some_and(|e| e == "toml") {
        return text.parse::<DocumentMut>().map_err(|e| refuse(&e));
    }
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).map_err(|e| refuse(&e))?;
    let table = match records::yaml_value_to_toml(&yaml) {
        Some(toml::Value::Table(t)) => t,
        _ => toml::Table::new(),
    };
    let rendered = toml::to_string(&table).map_err(|e| refuse(&e))?;
    rendered.parse::<DocumentMut>().map_err(|e| refuse(&e))
}

/// `save_providers` inside [mutate]: `records` and `active` replace the stored
/// ones; every other accounts subkey and every other top-level key rides. A
/// legacy `[config]` wrapper is lifted and a `providers` block is migrated
/// onto `accounts`.
pub fn set_records(doc: &mut DocumentMut, records: &[Record], active: Option<&str>) {
    let root = doc.as_table_mut();
    flatten_config(root);
    let accounts = match root.remove("accounts").map(Item::into_table) {
        Some(Ok(table)) => table,
        _ => {
            let mut table = Table::new();
            table.set_implicit(true);
            if let Some(old) = root.get("providers").and_then(Item::as_table_like) {
                for (key, item) in old.iter() {
                    if key != "records" && key != "active" {
                        table.insert(key, item.clone());
                    }
                }
            }
            table
        }
    };
    root.remove("providers");
    root.insert("accounts", Item::Table(accounts));
    let Some(Item::Table(accounts)) = root.get_mut("accounts") else {
        unreachable!("accounts was just inserted as a table");
    };
    let mut rows = ArrayOfTables::new();
    for record in records {
        rows.push(record_table(record));
    }
    accounts.insert("records", Item::ArrayOfTables(rows));
    match active {
        Some(active) => {
            accounts.insert("active", toml_edit::value(active));
        }
        None => {
            accounts.remove("active");
        }
    }
}

/// `loader.mutable_accounts_block` inside [mutate]: drain `config.accounts`,
/// `providers` and `config.providers`, keep a top-level `accounts` table or
/// adopt the first drained block, then lift the rest of `[config]`. Unlike
/// [set_records], a top-level block beats a wrapped one. Returns the warning
/// for each non-empty block it dropped.
pub fn accounts_block(doc: &mut DocumentMut) -> Vec<String> {
    let root = doc.as_table_mut();
    let mut drained: Vec<(&str, Table)> = Vec::new();
    for (in_config, key) in [
        (true, "accounts"),
        (false, "providers"),
        (true, "providers"),
    ] {
        let source = if in_config {
            root.get_mut("config").and_then(Item::as_table_like_mut)
        } else {
            Some(&mut *root as &mut dyn toml_edit::TableLike)
        };
        if let Some(Ok(found)) = source.and_then(|s| s.remove(key)).map(Item::into_table) {
            drained.push((key, found));
        }
    }
    let adopted = match root.remove("accounts").map(Item::into_table) {
        Some(Ok(table)) => table,
        _ if drained.is_empty() => Table::new(),
        _ => drained.remove(0).1,
    };
    root.insert("accounts", Item::Table(adopted));
    flatten_config(root);
    drained
        .iter()
        .filter(|(_, table)| !table.is_empty())
        .map(|(key, table)| {
            format!(
                "discarding shadowed account block '{key}' ({} key(s)); a higher-precedence \
                 block already provides config.accounts",
                table.len()
            )
        })
        .collect()
}

/// `loader._flatten_config`: a `config` table's keys move to the top level and
/// win over a top-level key of the same name.
fn flatten_config(root: &mut Table) {
    if !root.get("config").is_some_and(Item::is_table_like) {
        return;
    }
    if let Some(Ok(wrapper)) = root.remove("config").map(Item::into_table) {
        for (key, item) in wrapper {
            root.insert(&key, item);
        }
    }
}

/// One `[[accounts.records]]` row: the record dump with nulls and empty lists
/// dropped, as `save_providers` cleans it.
fn record_table(record: &Record) -> Table {
    let mut table = Table::new();
    let Json::Object(fields) = record.to_json() else {
        return table;
    };
    for (key, value) in fields {
        if value.is_null() || value.as_array().is_some_and(Vec::is_empty) {
            continue;
        }
        if let Some(value) = toml_value(&value) {
            table.insert(&key, Item::Value(value));
        }
    }
    table
}

/// A JSON value as TOML. A nested null is dropped (`_strip_none`); objects
/// render inline.
fn toml_value(value: &Json) -> Option<Value> {
    Some(match value {
        Json::Null => return None,
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::from(n.as_f64()?),
        },
        Json::String(s) => Value::from(s.as_str()),
        Json::Array(items) => Value::Array(items.iter().filter_map(toml_value).collect::<Array>()),
        Json::Object(map) => {
            let mut inline = InlineTable::new();
            for (key, item) in map {
                if let Some(item) = toml_value(item) {
                    inline.insert(key.as_str(), item);
                }
            }
            Value::InlineTable(inline)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_keeps_comments_and_refuses_an_unparseable_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "# keep me\n[review]\nmax_rounds = 2 # inline\n").unwrap();
        let save = |path: &Path, active: Option<&str>| {
            mutate(path, |doc| {
                set_records(doc, &[], active);
                Ok::<(), ()>(())
            })
        };
        save(&path, Some("work")).unwrap().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# keep me") && text.contains("max_rounds = 2 # inline"),
            "{text}"
        );
        assert!(text.contains("active = \"work\""), "{text}");
        assert!(lock_path(&path).is_file());

        std::fs::write(&path, "[review\n").unwrap();
        let err = save(&path, None).unwrap_err().to_string();
        assert!(
            err.starts_with("Cannot save: config file failed to parse ("),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[review\n");

        // An edit that refuses writes nothing.
        std::fs::write(&path, "x = 1\n").unwrap();
        let refused = mutate(&path, |doc| {
            doc.insert("y", toml_edit::value(2_i64));
            Err::<(), _>("refused")
        });
        assert_eq!(refused.unwrap(), Err("refused"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x = 1\n");

        // A write that cannot land leaves the file as it was (root ignores modes).
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let ro = tmp.path().join("ro");
            std::fs::create_dir(&ro).unwrap();
            std::fs::write(ro.join("config.toml"), "x = 1\n").unwrap();
            let writable = std::fs::metadata(&ro).unwrap().permissions();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
            let failed = save(&ro.join("config.toml"), Some("work"));
            std::fs::set_permissions(&ro, writable).unwrap();
            assert!(matches!(failed, Err(WriteError::Io(_))), "{failed:?}");
            assert_eq!(
                std::fs::read_to_string(ro.join("config.toml")).unwrap(),
                "x = 1\n"
            );
        }
    }
}
