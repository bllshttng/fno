//! The config writer: `fno config set`, `unset` and `rm`, native.
//!
//! A key resolves against the settings model's schema table, the value
//! coerces to the field's type (the CLI layer, deliberately looser than the
//! model's lax mode in a few spellings), and every block the batch touched
//! validates once against the final merged file. The read-modify-write runs
//! under one exclusive `<file>.lock`, re-reads under the lock, and replaces
//! the file atomically only when its parsed content changed. toml_edit keeps
//! the file's comments and key order, which the retired Python writer
//! (tomli_w) dropped; the parity goldens compare files parsed for that
//! reason. A merge-gating opt-out holds a `config-optout:<key>` claim through
//! the `fno-agents claim` doors, taken inside the file lock so the value and
//! its lease cannot drift. Each changed leaf appends one `config_write`
//! receipt; a receipt failure warns and never fails the write.
//!
//! Byte contract: the goldens under `tests/golden/config_set/`, captured from
//! the Python writer by `tests/config_set_driver.py`.

pub mod set;
pub mod unset;

use super::model::{self, py_repr};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A refusal the command prints as `error: <message>` and exits with.
#[derive(Debug)]
pub struct Refusal {
    pub message: String,
    pub code: i32,
}

impl Refusal {
    pub fn new(message: impl Into<String>, code: i32) -> Self {
        Refusal {
            message: message.into(),
            code,
        }
    }
}

pub type Res<T> = Result<T, Refusal>;

fn repr(text: &str) -> String {
    py_repr(&Value::String(text.to_string()))
}

// ---------------------------------------------------------------------------
// Key resolution over the schema table
// ---------------------------------------------------------------------------

/// A field's annotation, `Optional` unwrapped: the shapes the coercion and
/// the block validation branch on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    Model(String),
    /// `dict[str, Model]`: the next key segment is an entry name.
    MapOf(String),
    Dict,
    List,
    Bool,
    /// A union with a bool arm (`bool | dict[str, bool]`).
    BoolUnion,
    Int,
    Float,
    Other,
}

#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub shape: Shape,
    pub optional: bool,
}

fn ref_name(node: &Value) -> Option<String> {
    node.get("$ref")?
        .as_str()?
        .rsplit('/')
        .next()
        .map(str::to_string)
}

fn field_shape(node: &Value, default: Option<&Value>) -> Field {
    let mut node = node;
    let mut optional = false;
    if let Some(arms) = node.get("anyOf").and_then(Value::as_array) {
        let real: Vec<&Value> = arms
            .iter()
            .filter(|arm| arm.get("type").and_then(Value::as_str) != Some("null"))
            .collect();
        if real.len() == 1 && real.len() < arms.len() {
            optional = true;
            node = real[0];
        } else {
            let has_bool = real
                .iter()
                .any(|arm| arm.get("type").and_then(Value::as_str) == Some("boolean"));
            let shape = if has_bool {
                Shape::BoolUnion
            } else {
                Shape::Other
            };
            return Field {
                shape,
                optional: false,
            };
        }
    }
    let shape = if let Some(name) = ref_name(node) {
        Shape::Model(name)
    } else {
        match node.get("type").and_then(Value::as_str) {
            Some("object") => match node.get("additionalProperties").and_then(ref_name) {
                Some(name) => Shape::MapOf(name),
                None => Shape::Dict,
            },
            Some("array") => Shape::List,
            Some("boolean") => Shape::Bool,
            Some("integer") => Shape::Int,
            Some("number") => Shape::Float,
            // An unconstrained field with a list default (`lanes`) must not
            // store as a string.
            None if node.get("enum").is_none()
                && node.get("const").is_none()
                && default.is_some_and(Value::is_array) =>
            {
                Shape::List
            }
            _ => Shape::Other,
        }
    };
    Field { shape, optional }
}

fn field_at(model_name: &str, field: &str) -> Option<Field> {
    let node = model::field_node(model_name, field)?;
    Some(field_shape(
        &node,
        model::field_default(model_name, field).as_ref(),
    ))
}

/// Where a key is stored in the flat file: a leading `config.` drops and
/// the `providers` spelling stores under `accounts`.
pub(crate) fn storage_parts(key: &str) -> Vec<String> {
    let mut parts: Vec<String> = key.split('.').map(str::to_string).collect();
    if parts.first().map(String::as_str) == Some("config") {
        parts.remove(0);
    }
    if parts.first().map(String::as_str) == Some("providers") {
        parts[0] = "accounts".to_string();
    }
    parts
}

/// `_resolve_parent_block`: the model that owns the key's leaf and the
/// leaf's field, or `None` for an unknown key. One `dict[str, Model]` hop
/// is writable; a key that ends ON a map entry (a whole sub-block) is not.
pub(crate) fn resolve_parent_block(key: &str) -> Option<(String, Field)> {
    let parts = storage_parts(key);
    let (leaf, path) = parts.split_last()?;
    let mut owner = model::ROOT_MODEL.to_string();
    let mut map_model: Option<String> = None;
    for part in path {
        if let Some(inner) = map_model.take() {
            owner = inner;
            continue;
        }
        match field_at(&owner, part)?.shape {
            Shape::Model(name) => owner = name,
            Shape::MapOf(name) => map_model = Some(name),
            _ => return None,
        }
    }
    if map_model.is_some() {
        return None;
    }
    let field = field_at(&owner, leaf)?;
    Some((owner, field))
}

/// `resolve_dotted(SettingsModel(), parts)`: the value a removed key falls
/// back to, and whether it is a model block (printed the way pydantic's
/// `str()` prints a model). An absent `dict[str, Model]` entry reads as a
/// default-constructed entry.
pub(crate) fn default_at(key: &str) -> (Value, bool) {
    let mut owner = model::ROOT_MODEL.to_string();
    let mut value: Option<Value> = None;
    let mut map_model: Option<String> = None;
    let mut is_model = true;
    for part in storage_parts(key) {
        if let Some(inner) = map_model.take() {
            let entry = value.as_ref().and_then(|v| v.get(&part)).cloned();
            value = Some(entry.unwrap_or_else(|| model::model_defaults(&inner)));
            owner = inner;
            is_model = true;
            continue;
        }
        if !is_model {
            return (Value::Null, false);
        }
        let Some(field) = field_at(&owner, &part) else {
            return (Value::Null, false);
        };
        value = Some(match &value {
            None => model::field_default(&owner, &part).unwrap_or(Value::Null),
            Some(parent) => parent.get(&part).cloned().unwrap_or(Value::Null),
        });
        match field.shape {
            Shape::Model(name) => owner = name,
            Shape::MapOf(name) => {
                map_model = Some(name);
                is_model = false;
            }
            _ => is_model = false,
        }
    }
    (value.unwrap_or(Value::Null), is_model)
}

/// pydantic's `str(model)`: `field=repr(value)` pairs joined by spaces.
pub(crate) fn py_model_str(value: &Value) -> String {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| format!("{k}={}", py_repr(v)))
            .collect::<Vec<_>>()
            .join(" "),
        other => model::py_str(other),
    }
}

// ---------------------------------------------------------------------------
// Coercion
// ---------------------------------------------------------------------------

/// `int(text)`: whitespace, a sign and single underscores between digits.
fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let digits = t.trim_start_matches(['+', '-']);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    t.replace('_', "").parse::<i64>().ok()
}

fn coerce_list(value: &str) -> Res<Value> {
    let s = value.trim();
    if s.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    if s.starts_with(['[', '{', '"']) {
        let parsed: Value = serde_json::from_str(s).map_err(|_| {
            Refusal::new(
                format!(
                    "expected a list (JSON array or comma-separated); got {}",
                    repr(value)
                ),
                2,
            )
        })?;
        let Value::Array(items) = parsed else {
            return Err(Refusal::new(
                format!("expected a list; got {}", repr(value)),
                2,
            ));
        };
        return Ok(Value::Array(
            items
                .into_iter()
                .map(|item| {
                    if item.is_object() {
                        item
                    } else {
                        Value::String(model::py_str(&item))
                    }
                })
                .collect(),
        ));
    }
    Ok(Value::Array(
        s.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| Value::String(item.to_string()))
            .collect(),
    ))
}

/// `_coerce`: a CLI string to the field's type.
pub(crate) fn coerce(value: &str, field: &Field) -> Res<Value> {
    let lowered = value.trim().to_lowercase();
    if field.optional && matches!(lowered.as_str(), "null" | "none" | "") {
        return Ok(Value::Null);
    }
    match field.shape {
        Shape::Bool | Shape::BoolUnion => match lowered.as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ if field.shape == Shape::Bool => Err(Refusal::new(
                format!("expected a boolean (true/false); got {}", repr(value)),
                2,
            )),
            _ => Ok(Value::String(value.to_string())),
        },
        Shape::Int => py_int(value)
            .map(Value::from)
            .ok_or_else(|| Refusal::new(format!("expected an integer; got {}", repr(value)), 2)),
        Shape::Float => value
            .trim()
            .replace('_', "")
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| Refusal::new(format!("expected a number; got {}", repr(value)), 2)),
        Shape::List => coerce_list(value),
        _ => Ok(Value::String(value.to_string())),
    }
}

fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// A block or dict value: JSON first, then a YAML flow mapping.
fn parse_structured(value: &str, key: &str) -> Res<Value> {
    let s = value.trim();
    if let Ok(parsed) = serde_json::from_str::<Value>(s) {
        return Ok(parsed);
    }
    serde_yaml_ng::from_str::<serde_yaml_ng::Value>(s)
        .map(super::yaml_to_json)
        .map_err(|e| Refusal::new(format!("invalid JSON/YAML for {key}: {e}"), 2))
}

/// One key's final stored value. A block leaf validates here, alone; a
/// scalar or dict leaf names the parent block to validate on the final
/// state, so a batch setting cross-field-coupled keys is judged whole.
pub(crate) fn final_value(
    key: &str,
    raw: &str,
    owner: &str,
    field: &Field,
) -> Res<(Value, Option<(Vec<String>, String)>)> {
    let parts = storage_parts(key);
    let block_parts = parts[..parts.len() - 1].to_vec();
    let deferred = Some((block_parts, owner.to_string()));
    match &field.shape {
        Shape::Model(_) | Shape::MapOf(_) | Shape::Dict => {
            let parsed = parse_structured(raw, key)?;
            if !parsed.is_object() {
                return Err(Refusal::new(
                    format!(
                        "{} is a config block; expected a JSON/YAML object (mapping), got {}",
                        repr(key),
                        py_type_name(&parsed)
                    ),
                    2,
                ));
            }
            if let Shape::Model(name) = &field.shape {
                let (result, notes) = model::validate_model(name, &parsed);
                print_notes(&notes);
                if let Err(errors) = result {
                    let msg = errors.first().map(model::Error::msg).unwrap_or_default();
                    return Err(Refusal::new(format!("invalid value for {key}: {msg}"), 2));
                }
                return Ok((parsed, None));
            }
            Ok((parsed, deferred))
        }
        _ => Ok((coerce(raw, field)?, deferred)),
    }
}

fn print_notes(notes: &[String]) {
    for note in notes {
        eprintln!("{note}");
    }
}

/// `_validate_block`: the touched block of the final file against its model.
pub(crate) fn validate_block(data: &Value, block_parts: &[String], owner: &str) -> Res<()> {
    let block = get_nested(data, block_parts)
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let (result, notes) = model::validate_model(owner, &block);
    print_notes(&notes);
    let Err(errors) = result else {
        return Ok(());
    };
    let Some(first) = errors.first() else {
        return Ok(());
    };
    let loc = first.dotted_loc();
    let mut place = block_parts.join(".");
    if !loc.is_empty() {
        place = format!("{place}.{loc}");
    }
    Err(Refusal::new(
        format!("invalid value for {place}: {}", first.msg()),
        2,
    ))
}

// ---------------------------------------------------------------------------
// Tree edits
// ---------------------------------------------------------------------------

pub(crate) fn get_nested<'a>(data: &'a Value, parts: &[String]) -> Option<&'a Value> {
    let mut node = data;
    for part in parts {
        node = node.as_object()?.get(part)?;
    }
    Some(node)
}

pub(crate) fn deep_set(data: &mut Value, parts: &[String], value: Value) {
    let Some((leaf, head)) = parts.split_last() else {
        return;
    };
    let mut node = data;
    for part in head {
        if !node.is_object() {
            *node = Value::Object(Map::new());
        }
        let map = node.as_object_mut().expect("made an object above");
        if !map.get(part).is_some_and(Value::is_object) {
            map.insert(part.clone(), Value::Object(Map::new()));
        }
        node = map.get_mut(part).expect("inserted above");
    }
    if !node.is_object() {
        *node = Value::Object(Map::new());
    }
    node.as_object_mut()
        .expect("made an object above")
        .insert(leaf.clone(), value);
}

/// `_deep_unset`: remove the leaf and prune every block the removal left
/// empty. `None` when the key (or a parent block) is absent.
pub(crate) fn deep_unset(data: &mut Value, parts: &[String]) -> Option<Value> {
    let (head, rest) = parts.split_first()?;
    let map = data.as_object_mut()?;
    if rest.is_empty() {
        return map.remove(head);
    }
    let child = map.get_mut(head)?;
    if !child.is_object() {
        return None;
    }
    let was = deep_unset(child, rest)?;
    if child.as_object().is_some_and(Map::is_empty) {
        map.remove(head);
    }
    Some(was)
}

/// TOML has no null: a null leaf is an absent key, which loads as its
/// default.
fn strip_none(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_none(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .filter(|v| !v.is_null())
                .map(strip_none)
                .collect(),
        ),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// TOML rendering that keeps comments
// ---------------------------------------------------------------------------

fn toml_value(value: &Value) -> toml_edit::Value {
    match value {
        Value::Bool(b) => toml_edit::Value::from(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => toml_edit::Value::from(i),
            None => toml_edit::Value::from(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => toml_edit::Value::from(s.as_str()),
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(toml_value(item));
            }
            toml_edit::Value::Array(array)
        }
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (k, v) in map {
                table.insert(k.as_str(), toml_value(v));
            }
            toml_edit::Value::InlineTable(table)
        }
        Value::Null => toml_edit::Value::from(""),
    }
}

fn new_item(value: &Value) -> toml_edit::Item {
    match value {
        Value::Object(map) => {
            let mut table = toml_edit::Table::new();
            sync_table(&mut table, map);
            // A table holding only sub-tables prints no bare header.
            table.set_implicit(!map.is_empty() && map.values().all(Value::is_object));
            toml_edit::Item::Table(table)
        }
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
            let mut array = toml_edit::ArrayOfTables::new();
            for item in items {
                let mut table = toml_edit::Table::new();
                if let Value::Object(map) = item {
                    sync_table(&mut table, map);
                }
                array.push(table);
            }
            toml_edit::Item::ArrayOfTables(array)
        }
        other => toml_edit::Item::Value(toml_value(other)),
    }
}

fn value_json(value: &toml_edit::Value) -> Value {
    match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(items) => Value::Array(items.iter().map(value_json).collect()),
        toml_edit::Value::InlineTable(table) => Value::Object(
            table
                .iter()
                .map(|(k, v)| (k.to_string(), value_json(v)))
                .collect(),
        ),
    }
}

fn item_json(item: &toml_edit::Item) -> Value {
    match item {
        toml_edit::Item::None => Value::Null,
        toml_edit::Item::Value(v) => value_json(v),
        toml_edit::Item::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), item_json(v)))
                .collect(),
        ),
        toml_edit::Item::ArrayOfTables(a) => Value::Array(
            a.iter()
                .map(|t| {
                    Value::Object(
                        t.iter()
                            .map(|(k, v)| (k.to_string(), item_json(v)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    }
}

/// Bring `table` to `want`: stale keys leave, changed values replace, new
/// keys append, and an unchanged key keeps its formatting and comments.
fn sync_table(table: &mut toml_edit::Table, want: &Map<String, Value>) {
    let stale: Vec<String> = table
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !want.contains_key(k))
        .collect();
    for key in stale {
        table.remove(&key);
    }
    for (key, value) in want {
        match table.get_mut(key) {
            Some(item) => {
                if let (Value::Object(map), true) = (value, item.is_table()) {
                    if let Some(inner) = item.as_table_mut() {
                        sync_table(inner, map);
                    }
                    continue;
                }
                if item_json(item) != *value {
                    *item = new_item(value);
                }
            }
            None => {
                table.insert(key, new_item(value));
            }
        }
    }
}

/// `existing` (the file's text, when it had one) edited to hold `after`.
fn render(existing: Option<&str>, after: &Value) -> Result<String, String> {
    let mut doc = match existing {
        Some(text) => text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| e.to_string())?,
        None => toml_edit::DocumentMut::new(),
    };
    let empty = Map::new();
    sync_table(doc.as_table_mut(), after.as_object().unwrap_or(&empty));
    Ok(doc.to_string())
}

/// The file parsed for the writer, or tomllib's one-line refusal shape.
fn parse_toml(text: &str) -> Result<Value, String> {
    match text.parse::<toml_edit::DocumentMut>() {
        Ok(_) => toml::from_str::<toml::Value>(text)
            .map(super::toml_to_json)
            .map_err(|e| e.message().to_string()),
        Err(e) => {
            let at = e.span().map(|s| s.start).unwrap_or(0).min(text.len());
            let head = &text[..at];
            let line = head.matches('\n').count() + 1;
            let column = head.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            Err(format!(
                "{} (at line {line}, column {column})",
                e.message().trim()
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// The locked read-modify-write
// ---------------------------------------------------------------------------

pub(crate) fn lock_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Take `<file>.lock` exclusively: blocking, or polling inside `budget`.
pub(crate) fn lock_file(path: &Path, budget: Option<Duration>) -> Res<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let lock = std::fs::File::create(path)
        .map_err(|e| Refusal::new(format!("cannot open {}: {e}", path.display()), 1))?;
    let fd = lock.as_raw_fd();
    let Some(budget) = budget else {
        // SAFETY: the fd belongs to `lock`, which outlives the call.
        if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            return Err(Refusal::new(format!("cannot lock {}", path.display()), 1));
        }
        return Ok(lock);
    };
    let deadline = Instant::now() + budget;
    loop {
        // SAFETY: as above.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err(Refusal::new(
                format!(
                    "config lock timeout after {}s at {}",
                    budget.as_secs_f64(),
                    path.display()
                ),
                1,
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_atomic(target: &Path, text: &str) -> std::io::Result<()> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let temp = target.with_file_name(format!(".{name}.tmp.{}.{nanos}.part", std::process::id()));
    let result = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, target));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The file after one locked update: its real path.
pub(crate) struct Written {
    pub path: PathBuf,
}

/// `_locked_update`: lock, re-read, mutate, write only on a parsed change,
/// unlock, then append the receipts. A symlinked target is written through
/// to its referent, and the lock follows it, so a worktree write and a
/// canonical write serialize on one lock.
pub(crate) fn locked_update<F>(
    target: &Path,
    scope: &str,
    budget: Option<Duration>,
    mutate: F,
) -> Res<Written>
where
    F: FnOnce(Value) -> Res<Value>,
{
    let failed = |e: std::io::Error| {
        Refusal::new(
            format!(
                "failed to write {}: {e} (settings left unchanged)",
                target.display()
            ),
            1,
        )
    };
    let real = if super::is_symlink(target) {
        super::resolve_path(target)
    } else {
        target.to_path_buf()
    };
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let lock = lock_file(&lock_path(&real), budget)?;
    let text = match std::fs::read_to_string(&real) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(failed(e)),
    };
    let before = match &text {
        Some(text) => parse_toml(text).map_err(|e| {
            Refusal::new(
                format!("existing config at {} is malformed: {e}", real.display()),
                1,
            )
        })?,
        None => Value::Object(Map::new()),
    };
    let after = strip_none(mutate(before.clone())?);
    if after != before {
        let rendered = render(text.as_deref(), &after)
            .map_err(|e| Refusal::new(format!("failed to write {}: {e}", real.display()), 1))?;
        write_atomic(&real, &rendered).map_err(failed)?;
    }
    drop(lock);
    emit_receipts(&before, &after, &real, scope);
    Ok(Written { path: real })
}

// ---------------------------------------------------------------------------
// Target paths and the legacy yaml convert
// ---------------------------------------------------------------------------

fn global_dir() -> PathBuf {
    super::global_settings_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| super::home_dir().join(".fno"))
}

/// The file this scope writes. A legacy `settings.yaml` beside it converts
/// to a flat `config.toml` first, so an unmigrated install is converted,
/// never lost. A `.toml` `FNO_CONFIG` pin names the global file outright and is
/// never migrated, the way the loader reads it.
pub(crate) fn target_path(scope: &str) -> Res<PathBuf> {
    if scope == "global" {
        // A legacy yaml pin stays a read-only declaration for the loader; the
        // write then lands in the default global file, as it always did.
        if let Some(pin) = std::env::var_os("FNO_CONFIG")
            .map(PathBuf::from)
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        {
            return Ok(pin);
        }
    }
    let fno_dir = match scope {
        "project" => super::repo_root().join(".fno"),
        "global" => global_dir(),
        other => {
            return Err(Refusal::new(
                format!("unknown scope {}: use project or global", repr(other)),
                2,
            ))
        }
    };
    migrate_yaml(&fno_dir.join("settings.yaml"));
    Ok(fno_dir.join("config.toml"))
}

/// `_migrate_yaml_to_toml`: one-shot, idempotent, crash-safe. The toml is
/// written next to the REAL yaml (temp plus rename) and the yaml leaves
/// only after the toml is durable; a malformed yaml stays in place.
fn migrate_yaml(yaml: &Path) {
    let real = if super::is_symlink(yaml) {
        super::resolve_path(yaml)
    } else {
        yaml.to_path_buf()
    };
    let toml_path = real.with_file_name("config.toml");
    if toml_path.exists() || !real.is_file() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&real) else {
        return;
    };
    let parsed = match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) {
        Ok(parsed) => super::yaml_to_json(parsed),
        Err(_) => {
            eprintln!(
                "config migrate: {} is malformed; leaving it in place",
                real.display()
            );
            return;
        }
    };
    let flat = if parsed.is_object() {
        strip_none(super::unwrap_config_dict(&parsed))
    } else {
        Value::Object(Map::new())
    };
    let Ok(rendered) = render(None, &flat) else {
        return;
    };
    if write_atomic(&toml_path, &rendered).is_ok() {
        let _ = std::fs::remove_file(&real);
    }
}

// ---------------------------------------------------------------------------
// Receipts
// ---------------------------------------------------------------------------

/// The attester the process tree proves for this caller, from the one
/// resolver in fno-agents. No binary or no answer reads as an unproven,
/// empty identity.
pub(crate) fn attester_identity() -> (String, String) {
    let unproven = (String::new(), "env_only".to_string());
    let Some(binary) = crate::digest_overlay::fno_agents_bin_auto() else {
        return unproven;
    };
    let Ok(out) = Command::new(binary)
        .args(["attest", "identity", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return unproven;
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&out.stdout) else {
        return unproven;
    };
    let field = |name: &str| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    (
        field("session_id").unwrap_or_default(),
        field("witness").unwrap_or_else(|| "env_only".to_string()),
    )
}

const SECRET_SEGMENTS: [&str; 4] = ["key", "token", "secret", "password"];
const RECEIPT_VALUE_CAP: usize = 4096;

/// A leaf's receipt form: a secret-named key redacts to a digest, a long
/// value truncates, everything else rides as itself.
fn receipt_value(key: &str, value: &Value) -> (Value, bool) {
    use sha2::{Digest, Sha256};
    let serialized = serde_json::to_string(value).unwrap_or_default();
    if key
        .split('.')
        .any(|segment| SECRET_SEGMENTS.contains(&segment.to_lowercase().as_str()))
    {
        let digest: String = Sha256::digest(serialized.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        return (
            Value::String(format!("<redacted:sha256:{}>", &digest[..12])),
            true,
        );
    }
    let length = serialized.chars().count();
    if length > RECEIPT_VALUE_CAP {
        let head: String = serialized.chars().take(RECEIPT_VALUE_CAP).collect();
        let omitted = length - RECEIPT_VALUE_CAP;
        return (
            Value::String(format!("{head}...(truncated {omitted} chars)")),
            false,
        );
    }
    (value.clone(), false)
}

fn leaves(value: &Value) -> std::collections::BTreeMap<String, Value> {
    let mut out = Vec::new();
    super::flatten_leaf_paths(value, "", &mut out);
    out.into_iter().collect()
}

/// One `config_write` row per changed leaf, to the scope's journal. The
/// file is already replaced, so a journal failure warns and never fails the
/// write.
fn emit_receipts(before: &Value, after: &Value, real: &Path, scope: &str) {
    let before = leaves(before);
    let after = leaves(after);
    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    let changed: Vec<&String> = keys
        .into_iter()
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    if changed.is_empty() {
        return;
    }
    let (session, witness) = attester_identity();
    let journal = if scope == "project" {
        super::history::project_journal()
    } else {
        super::history::global_journal()
    };
    let config_path = super::resolve_path(real).to_string_lossy().to_string();
    let operator = super::resolve_path(&global_dir().join("config.toml"));
    let root_kind = if config_path == operator.to_string_lossy() {
        "operator"
    } else {
        "project"
    };
    let mut failed: Vec<&str> = Vec::new();
    let mut last_error = String::new();
    for key in changed {
        let mut data = Map::new();
        data.insert("key".into(), json!(key));
        data.insert("scope".into(), json!(scope));
        data.insert("root_kind".into(), json!(root_kind));
        data.insert("config_path".into(), json!(config_path));
        data.insert("present_before".into(), json!(before.contains_key(key)));
        data.insert("present_after".into(), json!(after.contains_key(key)));
        data.insert("attester_session_id".into(), json!(session));
        data.insert("attester_witness".into(), json!(witness));
        let mut redacted = false;
        if let Some(old) = before.get(key) {
            let (value, hidden) = receipt_value(key, old);
            data.insert("old_value".into(), value);
            redacted |= hidden;
        }
        if let Some(new) = after.get(key) {
            let (value, hidden) = receipt_value(key, new);
            data.insert("new_value".into(), value);
            redacted |= hidden;
        }
        if redacted {
            data.insert("redacted".into(), json!(true));
        }
        let envelope = json!({
            "ts": crate::setup_run::utc_now(),
            "type": "config_write",
            "source": "config",
            "data": Value::Object(data),
        });
        if let Err(e) = crate::event_store::append_envelope(&journal, &envelope.to_string(), None) {
            failed.push(key.as_str());
            last_error = e;
        }
    }
    if !failed.is_empty() {
        eprintln!(
            "warning: config write receipt not recorded for key(s): {}: {last_error}",
            failed.join(", ")
        );
    }
}

// ---------------------------------------------------------------------------
// The merge-gating opt-out lease lane
// ---------------------------------------------------------------------------

const OPTOUT_PREFIX: &str = "config-optout:";

pub(crate) fn is_merge_gating(key: &str) -> bool {
    model::MERGE_GATING_OPTOUTS.iter().any(|(k, _)| *k == key)
}

/// `_is_optout_value`: present and equal to the key's opt-out value.
pub(crate) fn optout_active(key: &str, value: Option<&Value>) -> bool {
    value.is_some_and(|v| model::is_optout_value(key, v))
}

/// The command a holder runs to release the opt-out at `key`.
pub(crate) fn optout_release_command(key: &str, scope_flag: &str) -> String {
    if key == "review.optional_apps" {
        return format!("fno config unset {key}{scope_flag}");
    }
    format!("fno config set {key} true{scope_flag}")
}

enum ClaimError {
    HeldByOther(String),
    Other(String),
}

impl ClaimError {
    fn text(self) -> String {
        match self {
            ClaimError::HeldByOther(text) | ClaimError::Other(text) => text,
        }
    }
}

/// One `fno-agents claim <op> <key> <flags> [--root R] --json` round trip,
/// decoded the way the retired Python claim client decoded it.
fn claim_call(op: &str, key: &str, flags: &[String]) -> Result<Value, ClaimError> {
    let binary = crate::digest_overlay::fno_agents_bin_auto().ok_or_else(|| {
        ClaimError::Other(
            "fno-agents claim unavailable: set FNO_AGENTS_BIN or reinstall fno".to_string(),
        )
    })?;
    let mut argv = vec!["claim".to_string(), op.to_string(), key.to_string()];
    argv.extend(flags.iter().cloned());
    if let Some(root) = std::env::var_os("FNO_CLAIMS_ROOT") {
        let root = root.to_string_lossy().trim().to_string();
        if !root.is_empty() {
            argv.push("--root".to_string());
            argv.push(root);
        }
    }
    argv.push("--json".to_string());
    let out = Command::new(binary)
        .args(&argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ClaimError::Other(format!("fno-agents claim could not run: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let payload: Value = if stdout.is_empty() {
        json!({})
    } else {
        serde_json::from_str(&stdout).map_err(|e| {
            ClaimError::Other(format!("fno-agents claim returned invalid JSON: {e}"))
        })?
    };
    let code = out.status.code().unwrap_or(-1);
    let text = |name: &str| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string()
    };
    if code == 1 && payload.get("outcome").and_then(Value::as_str) == Some("held_by_other") {
        let pid = py_repr(payload.get("pid").unwrap_or(&Value::Null));
        return Err(ClaimError::HeldByOther(format!(
            "claim {} held by {} (pid={pid}, host={})",
            repr(key),
            text("holder"),
            text("host")
        )));
    }
    if op == "status" && payload.get("state").and_then(Value::as_str) == Some("corrupted") {
        return Ok(payload);
    }
    if code != 0 {
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit {code}")
        };
        return Err(ClaimError::Other(format!(
            "fno-agents claim {op} failed: {detail}"
        )));
    }
    Ok(payload)
}

fn optout_status(key: &str) -> Res<Value> {
    claim_call("status", &format!("{OPTOUT_PREFIX}{key}"), &[])
        .map_err(|e| Refusal::new(e.text(), 1))
}

fn release(key: &str, holder: &str) -> Res<()> {
    let flags = [
        "--holder".to_string(),
        holder.to_string(),
        "--with-claim".to_string(),
    ];
    claim_call("release", &format!("{OPTOUT_PREFIX}{key}"), &flags)
        .map(|_| ())
        .map_err(|e| Refusal::new(e.text(), 1))
}

/// The session allowed to hold an opt-out lease: the proven attester.
fn optout_holder() -> Res<String> {
    let (session, _witness) = attester_identity();
    if session.is_empty() {
        return Err(Refusal::new(
            "merge-gating opt-out requires a resolved attester session",
            1,
        ));
    }
    Ok(session)
}

fn state_of(status: &Value) -> &str {
    status.get("state").and_then(Value::as_str).unwrap_or("")
}

fn live_or_suspect(status: &Value) -> bool {
    matches!(state_of(status), "live" | "suspect")
}

/// What a held lease reports on the `lease:` line.
#[derive(Clone, Debug)]
pub struct Lease {
    pub holder: String,
    pub expires_at: Value,
}

/// The lease bookkeeping one set carries: leases granted per key, the ones
/// this write newly took (rolled back if it fails), and the releases due
/// once it lands.
#[derive(Default)]
pub(crate) struct LeaseBook {
    pub leases: Vec<(String, Lease)>,
    pub newly_acquired: Vec<(String, String)>,
    pub releases: Vec<(String, String)>,
}

impl LeaseBook {
    pub(crate) fn rollback(&mut self) {
        for (key, holder) in std::mem::take(&mut self.newly_acquired) {
            let _ = release(&key, &holder);
        }
    }

    pub(crate) fn finalize(&mut self) -> Res<()> {
        for (key, holder) in std::mem::take(&mut self.releases) {
            release(&key, &holder)?;
        }
        Ok(())
    }

    pub(crate) fn lease(&self, key: &str) -> Option<&Lease> {
        self.leases.iter().find(|(k, _)| k == key).map(|(_, l)| l)
    }
}

fn optout_ttl_ms(data: &Value) -> Res<i64> {
    let parts = ["review".to_string(), "optout_ttl_minutes".to_string()];
    let minutes = match get_nested(data, &parts) {
        None => 60,
        Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or(60),
        Some(_) => {
            return Err(Refusal::new(
                "review.optout_ttl_minutes must be an integer between 1 and 1440",
                2,
            ))
        }
    };
    Ok(minutes * 60 * 1000)
}

/// `prepare_set_leases`, inside the file lock: acquire (or refresh) a lease
/// for every merge-gating key the write leaves opted out, and schedule the
/// release of every one it turns back on. A refusal mid-batch releases what
/// this batch already took.
pub(crate) fn prepare_leases(
    order: &[String],
    existing: &Value,
    data: &Value,
    target: &Path,
    scope: &str,
    book: &mut LeaseBook,
) -> Res<()> {
    let result = prepare_leases_inner(order, existing, data, target, scope, book);
    if result.is_err() {
        book.rollback();
    }
    result
}

fn prepare_leases_inner(
    order: &[String],
    existing: &Value,
    data: &Value,
    target: &Path,
    scope: &str,
    book: &mut LeaseBook,
) -> Res<()> {
    for key in order.iter().filter(|k| is_merge_gating(k)) {
        let parts = storage_parts(key);
        let before = get_nested(existing, &parts);
        let before_active = optout_active(key, before);
        let after_active = optout_active(key, get_nested(data, &parts));
        let status = optout_status(key)?;
        let claim_key = format!("{OPTOUT_PREFIX}{key}");
        if after_active {
            let holder = optout_holder()?;
            let config_path = if super::is_symlink(target) {
                super::resolve_path(target)
            } else {
                target.to_path_buf()
            };
            let prior_present = before.is_some() && !before_active;
            let prior_value = match before {
                Some(value) if prior_present => value.clone(),
                _ => Value::Null,
            };
            let mut metadata = json!({
                "config_key": key,
                "config_path": config_path.to_string_lossy(),
                "scope": scope,
                "prior_present": prior_present,
                "prior_value": prior_value,
            });
            // A refresh keeps the value the lease must restore, but only
            // when the old lease described THIS file.
            if let Some(old) = status.get("metadata").filter(|m| m.is_object()) {
                if old.get("config_key") == metadata.get("config_key")
                    && old.get("config_path") == metadata.get("config_path")
                    && matches!(state_of(&status), "live" | "suspect" | "stale")
                {
                    metadata = old.clone();
                }
            }
            let flags = vec![
                "--holder".to_string(),
                holder.clone(),
                "--ttl-ms".to_string(),
                optout_ttl_ms(data)?.to_string(),
                "--reason".to_string(),
                "merge-gating opt-out".to_string(),
                "--metadata".to_string(),
                metadata.to_string(),
                "--pid".to_string(),
                std::process::id().to_string(),
            ];
            let payload = claim_call("acquire", &claim_key, &flags).map_err(|e| match e {
                ClaimError::HeldByOther(text) => Refusal::new(text, 1),
                ClaimError::Other(text) => {
                    Refusal::new(format!("cannot acquire {claim_key}: {text}"), 1)
                }
            })?;
            let body = payload.get("claim").unwrap_or(&payload);
            book.leases.push((
                key.clone(),
                Lease {
                    holder: body
                        .get("holder")
                        .and_then(Value::as_str)
                        .unwrap_or(holder.as_str())
                        .to_string(),
                    expires_at: body.get("expires_at").cloned().unwrap_or(Value::Null),
                },
            ));
            let already_ours = live_or_suspect(&status)
                && status.get("holder").and_then(Value::as_str) == Some(holder.as_str());
            if !already_ours {
                book.newly_acquired.push((key.clone(), holder));
            }
        } else if live_or_suspect(&status) {
            let holder = optout_holder()?;
            let held_by = status.get("holder").and_then(Value::as_str).unwrap_or("");
            if held_by != holder {
                return Err(Refusal::new(
                    format!(
                        "claim {} held by {}; only its holder may release the opt-out",
                        repr(&claim_key),
                        model::py_str(status.get("holder").unwrap_or(&Value::Null))
                    ),
                    1,
                ));
            }
            book.releases.push((key.clone(), holder));
        }
    }
    Ok(())
}

/// `plan_unset_release`: the holder whose lease an unset of `key` releases,
/// refused when another session holds it.
pub(crate) fn plan_unset_release(key: &str) -> Res<Option<String>> {
    let status = optout_status(key)?;
    if !live_or_suspect(&status) {
        return Ok(None);
    }
    let holder = optout_holder()?;
    if status.get("holder").and_then(Value::as_str) != Some(holder.as_str()) {
        return Err(Refusal::new(
            format!(
                "claim {} held by {}; only its holder may release the opt-out",
                repr(&format!("{OPTOUT_PREFIX}{key}")),
                model::py_str(status.get("holder").unwrap_or(&Value::Null))
            ),
            1,
        ));
    }
    Ok(Some(holder))
}

pub(crate) fn release_optout(key: &str, holder: &str) -> Res<()> {
    release(key, holder)
}

// ---------------------------------------------------------------------------
// The reaper's restore
// ---------------------------------------------------------------------------

/// Restore the value a retired `config-optout:` claim recorded: the prior
/// value, or the key's absence, but only while the file still holds the
/// opt-out. The lock wait is bounded (2 s) so a background reap never
/// blocks behind a foreground set; a busy lock is one failure line.
pub fn restore_reaped_optout(claim_key: &str, metadata: &Map<String, Value>) -> Result<(), String> {
    if !claim_key.starts_with(OPTOUT_PREFIX) {
        return Ok(());
    }
    let config_key = match metadata.get("config_key").and_then(Value::as_str) {
        Some(key) if is_merge_gating(key) => key.to_string(),
        _ => {
            return Err(format!(
                "{claim_key} has invalid config_key restore metadata"
            ))
        }
    };
    let config_path = match metadata.get("config_path").and_then(Value::as_str) {
        Some(path) if !path.is_empty() => super::resolve_path(Path::new(path)),
        _ => return Err(format!("{claim_key} has no config_path restore metadata")),
    };
    let parts = storage_parts(&config_key);
    let prior_present = metadata
        .get("prior_present")
        .is_some_and(|v| v.as_bool().unwrap_or(!v.is_null()));
    let prior_value = metadata.get("prior_value").cloned().unwrap_or(Value::Null);
    if !config_path.exists() && !prior_present {
        return Ok(());
    }
    let restore = |mut existing: Value| {
        if !optout_active(&config_key, get_nested(&existing, &parts)) {
            return Ok(existing);
        }
        if prior_present {
            deep_set(&mut existing, &parts, prior_value);
        } else {
            deep_unset(&mut existing, &parts);
        }
        Ok(existing)
    };
    locked_update(
        &config_path,
        "global",
        Some(Duration::from_secs(2)),
        restore,
    )
    .map(|_| ())
    .map_err(|refusal| refusal.message)
}

// ---------------------------------------------------------------------------
// Shared argv parsing (click's grammar for these three leaves)
// ---------------------------------------------------------------------------

/// The parsed argv of `set` / `unset`: positionals, the scope, or the exit
/// a usage error or `--help` already decided.
pub(crate) enum Parsed {
    Args {
        positionals: Vec<String>,
        local: bool,
    },
    Exit(i32),
}

/// click's usage refusal, exit 2.
pub(crate) fn usage_error(command: &str, metavar: &str, message: &str) -> i32 {
    eprintln!("Usage: fno config {command} [OPTIONS] {metavar}");
    eprintln!("Try 'fno config {command} --help' for help.");
    eprintln!();
    eprintln!("Error: {message}");
    2
}

pub(crate) fn parse_argv(
    command: &str,
    metavar: &str,
    help: &str,
    args: &[std::ffi::OsString],
) -> Parsed {
    let usage = |message: String| Parsed::Exit(usage_error(command, metavar, &message));
    let mut positionals = Vec::new();
    let mut local = false;
    let mut options_done = false;
    for arg in args {
        let token = arg.to_string_lossy().to_string();
        if options_done || token == "-" || !token.starts_with('-') {
            positionals.push(token);
            continue;
        }
        match token.as_str() {
            "--" => options_done = true,
            "--help" => {
                print!("{help}");
                return Parsed::Exit(0);
            }
            "-l" | "--local" => local = true,
            "-g" | "--global" => local = false,
            short
                if !short.starts_with("--") && short[1..].chars().all(|c| c == 'l' || c == 'g') =>
            {
                local = short.ends_with('l');
            }
            other => return usage(format!("No such option: {other}")),
        }
    }
    if positionals.is_empty() {
        return usage(format!("Missing argument '{metavar}'."));
    }
    Parsed::Args { positionals, local }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_refuses_metadata_without_a_config_path() {
        let mut metadata = Map::new();
        metadata.insert("config_key".into(), json!("review.self_review_required"));
        let refused = restore_reaped_optout("config-optout:review.self_review_required", &metadata);
        assert_eq!(
            refused,
            Err(
                "config-optout:review.self_review_required has no config_path restore metadata"
                    .to_string()
            )
        );
    }
}
