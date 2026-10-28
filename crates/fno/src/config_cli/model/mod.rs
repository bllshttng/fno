//! The native settings model: the Python `SettingsModel.model_validate` the
//! loader runs on every config load (`fno/config/_loader.py`), ported.
//!
//! The merged config from `merged_config` is validated against the committed
//! schema table (`config_schema.json` `json_schema`: types, enums, bounds),
//! with each pydantic `field_validator` and `model_validator` ported as one
//! Rust function keyed by model and field (`validators_*.rs`). Field order,
//! defaults and default factories come from `fields.json`, which the oracle
//! driver (`tests/config_load_driver.py --emit-fields`) emits from pydantic:
//! the schema table sorts keys and drops factories, and both decide the
//! refusal order and the defaults dump.
//!
//! The rules are pydantic v2's lax mode as the goldens froze it: a bool
//! takes `yes`/`off`/0/1, an int takes a whole float and a numeric string, a
//! str takes only a str. A refusal names file, key, input repr and the
//! expected set, capped at five lines (`describe_config_failure`).

mod repr;
mod unknown;
mod validators_blocks;
mod validators_core;
mod validators_fleet;
mod validators_root;

pub use repr::{py_float, py_repr, py_str};

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const FIELDS: &str = include_str!("fields.json");
const SCHEMA: &str = include_str!("../../../config_schema.json");
const ROOT: &str = "SettingsModel";
const REFUSAL_ERROR_CAP: usize = 5;
const UNKNOWN_WARN_CAP: usize = 3;

/// One loc segment: a field or dict key, or a list index.
#[derive(Clone, Debug, PartialEq)]
pub enum Loc {
    Key(String),
    Index(usize),
}

impl Loc {
    fn render(&self) -> String {
        match self {
            Loc::Key(key) => key.clone(),
            Loc::Index(index) => index.to_string(),
        }
    }
}

/// One validation error, the fields of pydantic's `exc.errors()` the
/// refusal text reads: loc, input, and the expected-or-msg text.
#[derive(Clone, Debug)]
pub struct Error {
    pub loc: Vec<Loc>,
    pub input: Value,
    pub text: String,
    /// The text is a literal's expected set, which pydantic's `msg` prefixes
    /// with `Input should be ` (the refusal text prints the set alone).
    pub literal: bool,
}

impl Error {
    /// pydantic's `msg` for this error, the text `fno config set` prints.
    pub fn msg(&self) -> String {
        if self.literal {
            format!("Input should be {}", self.text)
        } else {
            self.text.clone()
        }
    }

    /// The dotted `loc`, the way the writer names the refused field.
    pub fn dotted_loc(&self) -> String {
        let parts: Vec<String> = self.loc.iter().map(Loc::render).collect();
        parts.join(".")
    }
}

/// A validator's failure. `Value` renders `Value error, <text>` (a raised
/// ValueError); `Custom` renders its text verbatim.
#[derive(Clone, Debug)]
pub enum Fail {
    Value(String),
    Custom(String),
}

impl Fail {
    fn text(self) -> String {
        match self {
            Fail::Value(text) => format!("Value error, {text}"),
            Fail::Custom(text) => text,
        }
    }
}

/// What a validator sees: the stderr lines the load prints so far, the
/// field it validates (`info.field_name`) and the fields validated before it
/// (`info.data`).
#[derive(Default)]
pub struct Ctx {
    pub notes: Vec<String>,
    pub field_name: String,
    pub data: Map<String, Value>,
    /// `model_fields_set` for a model-after validator: the fields the input
    /// supplied, defaults excluded.
    pub fields_set: Vec<String>,
}

impl Ctx {
    /// `_LOG.warning(text)` / `print(text, file=sys.stderr)`: one line.
    pub fn note(&mut self, text: impl Into<String>) {
        self.notes.push(text.into());
    }

    /// `warnings.warn(text)`: the category and the text; the host owns the
    /// file:line prefix Python prints before it.
    pub fn warn(&mut self, text: impl AsRef<str>) {
        self.notes.push(format!("UserWarning: {}", text.as_ref()));
    }
}

pub type FieldFn = fn(&mut Ctx, Value) -> Result<Value, Fail>;
pub type ModelBeforeFn = fn(&mut Ctx, Value) -> Result<Value, Fail>;
pub type ModelAfterFn = fn(&mut Ctx, &mut Map<String, Value>) -> Result<(), Fail>;

/// The ported validators, keyed the way pydantic binds them.
#[derive(Default)]
pub struct Registry {
    field_before: HashMap<(&'static str, &'static str), Vec<FieldFn>>,
    field_after: HashMap<(&'static str, &'static str), Vec<FieldFn>>,
    model_before: HashMap<&'static str, Vec<ModelBeforeFn>>,
    model_after: HashMap<&'static str, Vec<ModelAfterFn>>,
}

impl Registry {
    /// `@field_validator(<fields>, mode="before")` on `model`.
    pub fn before(&mut self, model: &'static str, fields: &[&'static str], f: FieldFn) {
        for field in fields {
            self.field_before.entry((model, field)).or_default().push(f);
        }
    }

    /// `@field_validator(<fields>)` (mode "after") on `model`.
    pub fn after(&mut self, model: &'static str, fields: &[&'static str], f: FieldFn) {
        for field in fields {
            self.field_after.entry((model, field)).or_default().push(f);
        }
    }

    /// `@model_validator(mode="before")`. Register a parent class's before
    /// the subclass's: pydantic wraps, so the last registered runs first.
    pub fn model_before(&mut self, model: &'static str, f: ModelBeforeFn) {
        self.model_before.entry(model).or_default().push(f);
    }

    /// `@model_validator(mode="after")`, run in registration order.
    pub fn model_after(&mut self, model: &'static str, f: ModelAfterFn) {
        self.model_after.entry(model).or_default().push(f);
    }
}

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| {
        let mut reg = Registry::default();
        validators_core::register(&mut reg);
        validators_fleet::register(&mut reg);
        validators_root::register(&mut reg);
        validators_blocks::register(&mut reg);
        reg
    })
}

struct FieldSpec {
    name: String,
    alias: Option<String>,
    default: Option<Value>,
}

struct ModelSpec {
    extra: String,
    fields: Vec<FieldSpec>,
}

fn models() -> &'static HashMap<String, ModelSpec> {
    static MODELS: OnceLock<HashMap<String, ModelSpec>> = OnceLock::new();
    MODELS.get_or_init(|| {
        let table: Value = serde_json::from_str(FIELDS).expect("fields.json parses");
        let mut out = HashMap::new();
        for (name, spec) in table["models"].as_object().into_iter().flatten() {
            let fields = spec["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| FieldSpec {
                    name: row["name"].as_str().unwrap_or_default().to_string(),
                    alias: row.get("alias").and_then(Value::as_str).map(str::to_string),
                    default: row.get("default").cloned(),
                })
                .collect();
            let extra = spec["extra"].as_str().unwrap_or("ignore").to_string();
            out.insert(name.clone(), ModelSpec { extra, fields });
        }
        out
    })
}

fn schema() -> &'static Value {
    static SCHEMA_DOC: OnceLock<Value> = OnceLock::new();
    SCHEMA_DOC.get_or_init(|| {
        let table: Value = serde_json::from_str(SCHEMA).expect("config_schema.json parses");
        table["json_schema"].clone()
    })
}

/// A model's JSON-schema properties: the root's, or one `$defs` entry's.
fn model_props(model: &str) -> Option<&'static Map<String, Value>> {
    let doc = schema();
    let node = if model == ROOT {
        doc
    } else {
        doc.get("$defs")?.get(model)?
    };
    node.get("properties")?.as_object()
}

fn ref_name(node: &Value) -> Option<&str> {
    node.get("$ref")?.as_str()?.rsplit('/').next()
}

// ---------------------------------------------------------------------------
// The validation walk
// ---------------------------------------------------------------------------

struct Walk<'a> {
    ctx: Ctx,
    errors: Vec<Error>,
    reg: &'a Registry,
}

impl<'a> Walk<'a> {
    fn push(&mut self, loc: &[Loc], input: &Value, text: impl Into<String>) {
        self.errors.push(Error {
            loc: loc.to_vec(),
            input: input.clone(),
            text: text.into(),
            literal: false,
        });
    }

    /// Validate `input` as model `name` at `loc`. `None` when it refused;
    /// the errors are already pushed.
    fn model(&mut self, name: &str, input: Value, loc: &[Loc]) -> Option<Value> {
        let reg = self.reg;
        let mut input = input;
        if let Some(befores) = reg.model_before.get(name) {
            for f in befores.iter().rev() {
                let saved = std::mem::take(&mut self.ctx.data);
                let result = f(&mut self.ctx, input.clone());
                self.ctx.data = saved;
                match result {
                    Ok(next) => input = next,
                    Err(fail) => {
                        self.push(loc, &input, fail.text());
                        return None;
                    }
                }
            }
        }
        let Value::Object(map) = &input else {
            self.push(
                loc,
                &input,
                format!("Input should be a valid dictionary or instance of {name}"),
            );
            return None;
        };
        let spec = models().get(name)?;
        let props = model_props(name);
        let before_errors = self.errors.len();
        let outer_data = std::mem::take(&mut self.ctx.data);
        let mut out = Map::new();
        let mut taken: Vec<&str> = Vec::new();
        for field in &spec.fields {
            let key = field.alias.as_deref().unwrap_or(&field.name);
            taken.push(key);
            let mut here = loc.to_vec();
            here.push(Loc::Key(key.to_string()));
            let Some(raw) = map.get(key) else {
                match &field.default {
                    Some(default) => {
                        out.insert(field.name.clone(), default.clone());
                    }
                    None => self.push(&here, &input, "Field required"),
                }
                continue;
            };
            let schema_node = props
                .and_then(|p| p.get(key))
                .cloned()
                .unwrap_or(Value::Object(Map::new()));
            if let Some(value) = self.field(name, &field.name, raw.clone(), &schema_node, &here) {
                self.ctx.data.insert(field.name.clone(), value.clone());
                out.insert(field.name.clone(), value);
            }
        }
        for (key, value) in map {
            if taken.contains(&key.as_str()) {
                continue;
            }
            match spec.extra.as_str() {
                "allow" => {
                    out.insert(key.clone(), value.clone());
                }
                "forbid" => {
                    let mut here = loc.to_vec();
                    here.push(Loc::Key(key.clone()));
                    self.push(&here, value, "Extra inputs are not permitted");
                }
                _ => {}
            }
        }
        self.ctx.data = outer_data;
        if self.errors.len() > before_errors {
            return None;
        }
        if let Some(afters) = reg.model_after.get(name) {
            self.ctx.fields_set = spec
                .fields
                .iter()
                .filter(|f| map.contains_key(f.alias.as_deref().unwrap_or(&f.name)))
                .map(|f| f.name.clone())
                .collect();
            for f in afters {
                if let Err(fail) = f(&mut self.ctx, &mut out) {
                    self.push(loc, &input, fail.text());
                    return None;
                }
            }
        }
        Some(Value::Object(out))
    }

    /// One field: before validators, the schema type, after validators.
    fn field(
        &mut self,
        model: &str,
        field: &str,
        raw: Value,
        node: &Value,
        loc: &[Loc],
    ) -> Option<Value> {
        let reg = self.reg;
        let key = (model_key(model), field_key(model, field));
        let mut value = raw;
        self.ctx.field_name = field.to_string();
        if let Some(befores) = key.0.zip(key.1).and_then(|k| reg.field_before.get(&k)) {
            for f in befores.iter().rev() {
                self.ctx.field_name = field.to_string();
                match f(&mut self.ctx, value.clone()) {
                    Ok(next) => value = next,
                    Err(fail) => {
                        self.push(loc, &value, fail.text());
                        return None;
                    }
                }
            }
        }
        // pydantic reports a refused after-validator against the field's
        // input, not the coerced value it validated.
        let input = value.clone();
        let mut value = self.typed(node, value, loc)?;
        if let Some(afters) = key.0.zip(key.1).and_then(|k| reg.field_after.get(&k)) {
            for f in afters {
                self.ctx.field_name = field.to_string();
                match f(&mut self.ctx, value.clone()) {
                    Ok(next) => value = next,
                    Err(fail) => {
                        self.push(loc, &input, fail.text());
                        return None;
                    }
                }
            }
        }
        Some(value)
    }

    /// Validate `value` against one JSON-schema node.
    fn typed(&mut self, node: &Value, value: Value, loc: &[Loc]) -> Option<Value> {
        if let Some(name) = ref_name(node) {
            return self.model(name, value, loc);
        }
        if let Some(members) = node.get("anyOf").and_then(Value::as_array) {
            return self.union(members, value, loc);
        }
        if let Some(choices) = node.get("enum").and_then(Value::as_array) {
            return self.literal(choices, value, loc);
        }
        if let Some(constant) = node.get("const") {
            return self.literal(std::slice::from_ref(constant), value, loc);
        }
        let ty = node.get("type").and_then(Value::as_str).unwrap_or("");
        let coerced = match ty {
            "" => return Some(value),
            "null" => match value {
                Value::Null => Ok(Value::Null),
                _ => Err("Input should be None"),
            },
            "boolean" => coerce_bool(&value),
            "integer" => coerce_int(&value),
            "number" => coerce_float(&value),
            "string" => match &value {
                Value::String(_) => Ok(value.clone()),
                _ => Err("Input should be a valid string"),
            },
            "array" => return self.array(node, value, loc),
            "object" => return self.object(node, value, loc),
            _ => Ok(value.clone()),
        };
        match coerced {
            Ok(done) => self.bounds(node, done, loc),
            Err(text) => {
                self.push(loc, &value, text);
                None
            }
        }
    }

    fn bounds(&mut self, node: &Value, value: Value, loc: &[Loc]) -> Option<Value> {
        if let Some(min) = node.get("minLength").and_then(Value::as_u64) {
            let len = value.as_str().map(|s| s.chars().count()).unwrap_or(0) as u64;
            if len < min {
                let unit = if min == 1 { "character" } else { "characters" };
                self.push(
                    loc,
                    &value,
                    format!("String should have at least {min} {unit}"),
                );
                return None;
            }
        }
        let number = value.as_f64();
        let checks: [(&str, &str, fn(f64, f64) -> bool); 4] = [
            ("minimum", "greater than or equal to", |v, b| v >= b),
            ("maximum", "less than or equal to", |v, b| v <= b),
            ("exclusiveMinimum", "greater than", |v, b| v > b),
            ("exclusiveMaximum", "less than", |v, b| v < b),
        ];
        for (keyword, words, ok) in checks {
            if let (Some(bound), Some(number)) = (node.get(keyword), number) {
                if !ok(number, bound.as_f64().unwrap_or(0.0)) {
                    self.push(
                        loc,
                        &value,
                        format!("Input should be {words} {}", py_repr(bound)),
                    );
                    return None;
                }
            }
        }
        Some(value)
    }

    fn array(&mut self, node: &Value, value: Value, loc: &[Loc]) -> Option<Value> {
        let Value::Array(items) = &value else {
            self.push(loc, &value, "Input should be a valid list");
            return None;
        };
        let item_schema = node
            .get("items")
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        let before = self.errors.len();
        let mut out = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let mut here = loc.to_vec();
            here.push(Loc::Index(index));
            if let Some(done) = self.typed(&item_schema, item.clone(), &here) {
                out.push(done);
            }
        }
        (self.errors.len() == before).then_some(Value::Array(out))
    }

    fn object(&mut self, node: &Value, value: Value, loc: &[Loc]) -> Option<Value> {
        let Value::Object(map) = &value else {
            self.push(loc, &value, "Input should be a valid dictionary");
            return None;
        };
        let value_schema = match node.get("additionalProperties") {
            Some(Value::Object(schema)) => Value::Object(schema.clone()),
            _ => Value::Object(Map::new()),
        };
        let before = self.errors.len();
        let mut out = Map::new();
        for (key, entry) in map {
            let mut here = loc.to_vec();
            here.push(Loc::Key(key.clone()));
            if let Some(done) = self.typed(&value_schema, entry.clone(), &here) {
                out.insert(key.clone(), done);
            }
        }
        (self.errors.len() == before).then_some(Value::Object(out))
    }

    fn literal(&mut self, choices: &[Value], value: Value, loc: &[Loc]) -> Option<Value> {
        if choices.iter().any(|choice| literal_eq(choice, &value)) {
            return Some(value);
        }
        let reprs: Vec<String> = choices.iter().map(py_repr).collect();
        let expected = match reprs.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
            Some((last, _)) => last.clone(),
            None => String::new(),
        };
        self.push(loc, &value, expected);
        if let Some(last) = self.errors.last_mut() {
            last.literal = true;
        }
        None
    }

    /// `Optional[X]` takes None or an X, and its errors are X's alone. A
    /// wider union takes the first member whose type matches exactly, then
    /// the first that validates; a refusal lists each member's errors under
    /// the member's tag, the way pydantic's smart union reports them.
    fn union(&mut self, members: &[Value], value: Value, loc: &[Loc]) -> Option<Value> {
        let non_null: Vec<&Value> = members
            .iter()
            .filter(|m| m.get("type").and_then(Value::as_str) != Some("null"))
            .collect();
        let nullable = non_null.len() < members.len();
        if nullable && value.is_null() {
            return Some(Value::Null);
        }
        if non_null.len() == 1 {
            return self.typed(non_null[0], value, loc);
        }
        if let Some(exact) = non_null.iter().find(|m| exact_type(m, &value)) {
            let mut probe = self.fork();
            if let Some(done) = probe.typed(exact, value.clone(), loc) {
                self.ctx.notes.extend(probe.ctx.notes);
                return Some(done);
            }
        }
        let mut collected = Vec::new();
        for member in &non_null {
            let mut probe = self.fork();
            let mut here = loc.to_vec();
            here.push(Loc::Key(member_tag(member)));
            match probe.typed(member, value.clone(), &here) {
                Some(done) => {
                    self.ctx.notes.extend(probe.ctx.notes);
                    return Some(done);
                }
                None => collected.extend(probe.errors),
            }
        }
        self.errors.extend(collected);
        None
    }

    /// A sibling walk for one union member: it shares the registry, not a
    /// borrow of this walk, so the caller can merge its notes back.
    fn fork(&self) -> Walk<'a> {
        Walk {
            ctx: Ctx {
                notes: Vec::new(),
                field_name: self.ctx.field_name.clone(),
                data: self.ctx.data.clone(),
                fields_set: Vec::new(),
            },
            errors: Vec::new(),
            reg: self.reg,
        }
    }
}

/// Whether `value` validates as model `name`, for a validator whose Python
/// body builds a sub-model and falls back to defaults when it refuses.
pub fn validates_as(name: &str, value: &Value) -> bool {
    let mut walk = Walk {
        ctx: Ctx::default(),
        errors: Vec::new(),
        reg: registry(),
    };
    walk.model(name, value.clone(), &[]).is_some() && walk.errors.is_empty()
}

/// `<name>.model_validate(value)` for the writer: the validated value or
/// every error in pydantic's order, plus the lines the validators printed.
pub fn validate_model(name: &str, value: &Value) -> (Result<Value, Vec<Error>>, Vec<String>) {
    let mut walk = Walk {
        ctx: Ctx::default(),
        errors: Vec::new(),
        reg: registry(),
    };
    let done = walk.model(name, value.clone(), &[]);
    let result = match done {
        Some(value) if walk.errors.is_empty() => Ok(value),
        _ => Err(walk.errors),
    };
    (result, walk.ctx.notes)
}

/// The settings root's model name, where every dotted key starts.
pub const ROOT_MODEL: &str = ROOT;

/// The JSON-schema node of `model`'s field named `field` (its Python name,
/// not its alias): the key resolution the writer walks. `None` when the
/// model has no such field.
pub fn field_node(model: &str, field: &str) -> Option<Value> {
    let spec = models().get(model)?;
    let row = spec.fields.iter().find(|f| f.name == field)?;
    let key = row.alias.as_deref().unwrap_or(&row.name);
    Some(
        model_props(model)
            .and_then(|props| props.get(key))
            .cloned()
            .unwrap_or(Value::Object(Map::new())),
    )
}

/// The field's dumped default (a factory called once), `None` when required.
pub fn field_default(model: &str, field: &str) -> Option<Value> {
    let spec = models().get(model)?;
    spec.fields
        .iter()
        .find(|f| f.name == field)?
        .default
        .clone()
}

/// A default-constructed `model`, dumped: every field with a default, in
/// field order.
pub fn model_defaults(model: &str) -> Value {
    let mut out = Map::new();
    for field in models().get(model).map(|m| m.fields.as_slice()).unwrap_or(&[]) {
        if let Some(default) = &field.default {
            out.insert(field.name.clone(), default.clone());
        }
    }
    Value::Object(out)
}

/// Registry keys are `&'static str`; resolve the walk's names onto them.
fn model_key(model: &str) -> Option<&'static str> {
    models().get_key_value(model).map(|(k, _)| k.as_str())
}

fn field_key(model: &str, field: &str) -> Option<&'static str> {
    models()
        .get(model)?
        .fields
        .iter()
        .find(|f| f.name == field)
        .map(|f| f.name.as_str())
}

fn literal_eq(choice: &Value, value: &Value) -> bool {
    match (choice, value) {
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

fn exact_type(member: &Value, value: &Value) -> bool {
    if ref_name(member).is_some() {
        return value.is_object();
    }
    match (member.get("type").and_then(Value::as_str), value) {
        (Some("string"), Value::String(_)) => member.get("enum").is_none(),
        (Some("boolean"), Value::Bool(_)) => true,
        (Some("integer"), Value::Number(n)) => n.is_i64() || n.is_u64(),
        (Some("number"), Value::Number(n)) => n.is_f64(),
        (Some("array"), Value::Array(_)) => true,
        (Some("object"), Value::Object(_)) => true,
        _ => false,
    }
}

fn member_tag(member: &Value) -> String {
    if let Some(name) = ref_name(member) {
        return name.to_string();
    }
    match member.get("type").and_then(Value::as_str) {
        Some("string") => "str".into(),
        Some("integer") => "int".into(),
        Some("number") => "float".into(),
        Some("boolean") => "bool".into(),
        Some("array") => {
            let inner = member
                .get("items")
                .map(member_tag)
                .unwrap_or_else(|| "any".into());
            format!("list[{inner}]")
        }
        Some("object") => {
            let inner = match member.get("additionalProperties") {
                Some(schema @ Value::Object(_)) => member_tag(schema),
                _ => "any".into(),
            };
            format!("dict[str,{inner}]")
        }
        _ => "any".into(),
    }
}

/// pydantic's lax bool: the bool itself, 0/1 as int or float, and the
/// fixed word set in any case.
pub fn coerce_bool(value: &Value) -> Result<Value, &'static str> {
    const PARSE: &str = "Input should be a valid boolean, unable to interpret input";
    match value {
        Value::Bool(_) => Ok(value.clone()),
        // A fractional float is a type miss; a whole number past 0/1 a parse miss.
        Value::Number(n) => match n.as_f64() {
            Some(f) if f == 0.0 => Ok(Value::Bool(false)),
            Some(f) if f == 1.0 => Ok(Value::Bool(true)),
            Some(f) if f.fract() != 0.0 => Err("Input should be a valid boolean"),
            _ => Err(PARSE),
        },
        // pydantic does not strip the string first: " yes " refuses.
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(Value::Bool(false)),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(Value::Bool(true)),
            _ => Err(PARSE),
        },
        _ => Err("Input should be a valid boolean"),
    }
}

/// pydantic's lax int: an int, a bool, a whole float, a numeric string.
pub fn coerce_int(value: &Value) -> Result<Value, &'static str> {
    match value {
        Value::Bool(b) => Ok(Value::from(i64::from(*b))),
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(value.clone()),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            if !f.is_finite() {
                Err("Input should be a finite number")
            } else if f.fract() != 0.0 {
                Err("Input should be a valid integer, got a number with a fractional part")
            } else {
                Ok(Value::from(f as i64))
            }
        }
        Value::String(s) => {
            const PARSE: &str =
                "Input should be a valid integer, unable to parse string as an integer";
            let trimmed = s.trim().replace('_', "");
            if let Ok(int) = trimmed.parse::<i64>() {
                return Ok(Value::from(int));
            }
            // A whole decimal string ("3.0") is an int to pydantic.
            match trimmed.parse::<f64>() {
                Ok(f) if f.is_finite() && f.fract() == 0.0 && trimmed.contains('.') => {
                    Ok(Value::from(f as i64))
                }
                _ => Err(PARSE),
            }
        }
        _ => Err("Input should be a valid integer"),
    }
}

/// pydantic's lax float: any number or bool, a numeric string.
pub fn coerce_float(value: &Value) -> Result<Value, &'static str> {
    let float = |f: f64| {
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .ok_or("Input should be a finite number")
    };
    match value {
        Value::Bool(b) => float(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => float(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => match s.trim().parse::<f64>() {
            Ok(f) => float(f),
            Err(_) => Err("Input should be a valid number, unable to parse string as a number"),
        },
        _ => Err("Input should be a valid number"),
    }
}

// ---------------------------------------------------------------------------
// The loader
// ---------------------------------------------------------------------------

/// What a load printed and returned: the stderr lines in order, and the
/// validated, defaults-filled settings or the refusal text.
pub struct Outcome {
    pub notes: Vec<String>,
    pub result: Result<Value, String>,
}

/// Validate the merged config the loader built from `layers` (priority
/// order, each already aliased and unwrapped). `json_surface` silences the
/// unknown-key lines the way a `--json` argv does. `claim_state` answers a
/// claim key's state word, the read the opt-out revocation makes.
pub fn validate_layers(
    layers: &[(PathBuf, Value)],
    merged: Value,
    json_surface: bool,
    claim_state: &dyn Fn(&str) -> String,
) -> Outcome {
    let mut notes = Vec::new();
    let mut raw = super::unwrap_config_dict(&merged);
    if !json_surface {
        let problems = unknown::unknown_key_problems(layers);
        for line in problems.iter().take(UNKNOWN_WARN_CAP) {
            notes.push(format!("fno config: {line}"));
        }
        if problems.len() > UNKNOWN_WARN_CAP {
            notes.push(format!(
                "fno config: ... and {} more unknown config key(s); `fno config doctor` names them all",
                problems.len() - UNKNOWN_WARN_CAP
            ));
        }
    }
    revoke_unbacked_optouts(&mut raw, claim_state, &mut notes);
    let mut walk = Walk {
        ctx: Ctx::default(),
        errors: Vec::new(),
        reg: registry(),
    };
    let validated = walk.model(ROOT, raw, &[]);
    notes.extend(walk.ctx.notes);
    let result = match validated {
        Some(value) if walk.errors.is_empty() => Ok(value),
        _ => Err(describe_failure(&walk.errors, layers)),
    };
    Outcome { notes, result }
}

/// Validate one priority-ordered layer list with no worktree-local file:
/// the merge `merged_config` runs, without the disk walk.
pub fn validate_layer_list(
    layers: &[(PathBuf, Value)],
    json_surface: bool,
    claim_state: &dyn Fn(&str) -> String,
) -> Outcome {
    let mut merged = Value::Object(Map::new());
    for (_path, parsed) in layers.iter().rev() {
        super::deep_merge(&mut merged, parsed);
    }
    validate_layers(layers, merged, json_surface, claim_state)
}

/// The native `load_settings`: drop retired keys from disk, merge the
/// chain, print the unknown-key lines and validator notes to stderr, and
/// return the validated settings or the refusal text.
pub fn load_validated(repo_root: &Path) -> Result<Value, String> {
    let candidates = super::candidate_paths(repo_root);
    for receipt in drop_retired_dispatch_keys(&candidates) {
        eprintln!("fno config: {receipt}");
    }
    let layers = super::aliased_layers(repo_root);
    let merged = super::merged_config(repo_root);
    let json_surface = std::env::args().any(|a| a == "--json" || a == "-J");
    let outcome = validate_layers(&layers, merged, json_surface, &native_claim_state);
    for line in &outcome.notes {
        eprintln!("{line}");
    }
    outcome.result
}

/// Merge-gating opt-outs (`fno/config/optouts.py`): the opt-out value, and
/// the resolved default the revocation note names.
pub const MERGE_GATING_OPTOUTS: [(&str, &str); 3] = [
    ("review.self_review_required", "True"),
    ("review.optional_apps", "'built-in optional reviewer list'"),
    ("auto_merge.require_checks_pass", "True"),
];

pub fn is_optout_value(key: &str, value: &Value) -> bool {
    if key == "review.optional_apps" {
        return value.as_array().is_some_and(Vec::is_empty);
    }
    // Python compares `value != False`, and 0 == False there.
    match value {
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// `optout_lease.revoke_unbacked_optouts`: a merge-gating opt-out holds only
/// while its `config-optout:<key>` claim is live. Any other state, an
/// unreadable one included, drops the value so the default gate applies.
fn revoke_unbacked_optouts(
    raw: &mut Value,
    claim_state: &dyn Fn(&str) -> String,
    notes: &mut Vec<String>,
) {
    for (key, default) in MERGE_GATING_OPTOUTS {
        let (block, leaf) = key.split_once('.').unwrap_or((key, key));
        let present = raw
            .get(block)
            .and_then(|b| b.get(leaf))
            .is_some_and(|v| is_optout_value(key, v));
        if !present {
            continue;
        }
        let state = claim_state(&format!("config-optout:{key}"));
        if state == "live" {
            continue;
        }
        if let Some(Value::Object(table)) = raw.get_mut(block) {
            table.remove(leaf);
            if table.is_empty() {
                if let Some(map) = raw.as_object_mut() {
                    map.remove(block);
                }
            }
        }
        notes.push(format!(
            "merge-gating opt-out {key} revoked: claim instrument is {state}; resolved default is {default}"
        ));
    }
}

/// The claim state word from `fno-agents claim status <key> --json`, the
/// one claim reader; no binary, a failed run or no state word reads as
/// `unreadable`, which revokes.
pub fn native_claim_state(key: &str) -> String {
    let Some(binary) = crate::digest_overlay::fno_agents_bin_auto() else {
        return "unreadable".to_string();
    };
    let output = std::process::Command::new(binary)
        .args(["claim", "status", key, "--json"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    output
        .ok()
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
        .and_then(|payload| {
            payload
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unreadable".to_string())
}

fn describe_failure(errors: &[Error], layers: &[(PathBuf, Value)]) -> String {
    let mut lines = Vec::new();
    for error in errors.iter().take(REFUSAL_ERROR_CAP) {
        let key: Vec<String> = error.loc.iter().map(Loc::render).collect();
        let holder = holder_file(layers, &error.loc);
        lines.push(format!(
            "{holder}: {} = {}; {}",
            key.join("."),
            py_repr(&error.input),
            error.text
        ));
    }
    if lines.is_empty() {
        return "refusing to load settings: invalid settings".to_string();
    }
    if errors.len() > REFUSAL_ERROR_CAP {
        lines.push(format!("... and {} more", errors.len() - REFUSAL_ERROR_CAP));
    }
    format!("refusing to load settings:\n  {}", lines.join("\n  "))
}

/// The first priority-ordered layer holding the key; one trailing segment
/// may drop, since legacy coercion derives a deeper key than the file stores.
fn holder_file(layers: &[(PathBuf, Value)], loc: &[Loc]) -> String {
    if loc.is_empty() {
        return "settings".to_string();
    }
    for (path, parsed) in layers {
        let flat = super::unwrap_config_dict(parsed);
        if path_held(&flat, loc) || path_held(&flat, &loc[..loc.len() - 1]) {
            return path.display().to_string();
        }
    }
    "settings".to_string()
}

fn path_held(node: &Value, loc: &[Loc]) -> bool {
    let mut node = node;
    for part in loc {
        node = match (part, node) {
            (Loc::Index(index), Value::Array(items)) => match items.get(*index) {
                Some(next) => next,
                None => return false,
            },
            (part, Value::Object(map)) => match map.get(&part.render()) {
                Some(next) => next,
                None => return false,
            },
            _ => return false,
        };
    }
    true
}

/// Retired dispatch keys leave disk on every load, once
/// (the loader's one disk dropper): a TOML file whose text names one has
/// the key removed, an emptied table with it, and each removal returns one
/// receipt line. The writer only rewrites TOML, so a YAML file keeps the
/// key and prints nothing.
pub const RETIRED_DISPATCH_KEYS: [&str; 2] = ["dispatch.allowed_verbs", "dispatch.verb_registry"]; // verb-wording: allow

pub fn drop_retired_dispatch_keys(paths: &[PathBuf]) -> Vec<String> {
    let mut gone = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real.clone());
        if matches!(
            real.extension().and_then(|e| e.to_str()),
            Some("yaml" | "yml")
        ) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&real) else {
            continue;
        };
        if !RETIRED_DISPATCH_KEYS
            .iter()
            .any(|k| text.contains(k.split('.').nth(1).unwrap_or(k)))
        {
            continue;
        }
        for key in locked_drop(&real) {
            gone.push(format!("removed retired key {key} from {}", real.display()));
        }
    }
    gone
}

/// The drop under the writer's lock (`<file>.lock`, flock, a 2 s budget):
/// re-read under the lock, then replace the file atomically, so a
/// concurrent `config set` is never clobbered and a failed write never
/// truncates. A busy lock skips the drop this load.
fn locked_drop(path: &Path) -> Vec<&'static str> {
    let budget = Some(std::time::Duration::from_secs(2));
    let Ok(lock) = super::write::lock_file(&super::write::lock_path(path), budget) else {
        return Vec::new();
    };
    let removed = match std::fs::read_to_string(path) {
        Ok(text) => drop_from_toml(path, &text),
        Err(_) => Vec::new(),
    };
    drop(lock);
    removed
}

fn drop_from_toml(path: &Path, text: &str) -> Vec<&'static str> {
    let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for key in RETIRED_DISPATCH_KEYS {
        let (block, leaf) = key.split_once('.').unwrap_or((key, key));
        let Some(table) = doc.get_mut(block).and_then(|item| item.as_table_like_mut()) else {
            continue;
        };
        if table.remove(leaf).is_none() {
            continue;
        }
        removed.push(key);
        if table.is_empty() {
            doc.remove(block);
        }
    }
    if removed.is_empty() {
        return removed;
    }
    let mut temp_name = path.as_os_str().to_owned();
    temp_name.push(format!(".tmp-{}", std::process::id()));
    let temp = PathBuf::from(temp_name);
    if std::fs::write(&temp, doc.to_string()).is_err() || std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
        return Vec::new();
    }
    removed
}
