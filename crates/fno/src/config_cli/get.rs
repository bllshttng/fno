//! `fno config get <key> [--json]`: one resolved config value, native.
//!
//! The plain read; `config get --defaults` stays with `config_defaults`. The
//! value resolves over the loader-replay merge in [`super`], with the schema
//! table (`config_schema.json`, generated from the pydantic model) supplying
//! defaults and field order, so a block read renders the same defaults-filled
//! JSON the model's `model_dump_json` printed. The source line on stderr
//! replays `resolve_source` decision for decision. Byte contract: the goldens
//! under `tests/golden/config_cli/`, captured from the Python leg before its
//! deletion (the committed oracle driver owns the capture).

use super::{candidate_paths, repo_root, resolve_path, resolve_source};
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The captured `--help` page, byte for byte as typer printed it.
const HELP: &str = concat!(
    "Usage: fno config get [OPTIONS] KEY\n",
    "\n",
    "  Print a single resolved config value. Read-only.\n",
    "\n",
    "  Traverses the loaded settings model by dotted path so a skill / LLM caller can\n",
    "  read one value (e.g. the decomposition ceiling fallback) without re-\n",
    "  implementing settings lookup. Scalars print bare; nested objects print as\n",
    "  JSON. Unknown keys exit non-zero.\n",
    "\n",
    "  The leading ``config.`` is optional: a bare ``review.required_bots`` is\n",
    "  retried as ``config.review.required_bots`` so a caller need not remember the\n",
    "  redundant prefix (E: the review gate defaults to\n",
    "  ``config.review.required_bots`` but the shorthand used to error).\n",
    "\n",
    "  Which FILE decided the value prints on STDERR : the silent home-vs-project\n",
    "  override is the defect this fixes, so the resolved value alone on stdout would\n",
    "  keep the confusion. stdout stays value-only because callers pipe it\n",
    "  (normalize.sh compares the whole stream); the source line, including an\n",
    "  ``overrides`` clause exactly when a lower-precedence file set a value the\n",
    "  merge discarded, is stderr-only. ``--json`` carries both streams' facts as one\n",
    "  object.\n",
    "\n",
    "Arguments:\n",
    "  KEY  Dotted config key, e.g. config.blueprint.max_prs_per_epic  [required]\n",
    "\n",
    "Options:\n",
    "  -J, --json  Emit {key, value, source, overrides} as one JSON object on stdout.\n",
    "  --help      Show this message and exit.\n",
    "",
);

pub fn run(args: &[OsString]) -> i32 {
    let tokens: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let mut json_out = false;
    let mut key: Option<String> = None;
    // Click parses left to right and its errors stop at the first bad token,
    // so the walk below mirrors that order.
    for token in &tokens {
        match token.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return 0;
            }
            "--json" | "-J" => json_out = true,
            t if t.starts_with('-') && t != "-" => {
                eprintln!("Usage: fno config get [OPTIONS] KEY");
                eprintln!("Try 'fno config get --help' for help.");
                eprintln!();
                eprintln!("Error: No such option: {t}");
                return 2;
            }
            t => {
                if key.is_some() {
                    eprintln!("Usage: fno config get [OPTIONS] KEY");
                    eprintln!("Try 'fno config get --help' for help.");
                    eprintln!();
                    eprintln!("Error: Got unexpected extra argument ({t})");
                    return 2;
                }
                key = Some(t.to_string());
            }
        }
    }
    let Some(key) = key else {
        eprintln!("Usage: fno config get [OPTIONS] KEY");
        eprintln!("Try 'fno config get --help' for help.");
        eprintln!();
        eprintln!("Error: Missing argument 'KEY'.");
        return 2;
    };
    get_value(&key, json_out)
}

// ---------------------------------------------------------------------------
// The schema table walk
// ---------------------------------------------------------------------------

fn schema_doc() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| {
        serde_json::from_str(include_str!("../../config_schema.json"))
            .expect("config_schema.json is a committed, generated artifact")
    })
}

/// One schema position the dotted walk can land on.
enum Node {
    /// A scalar (or unconstrained) slot; carries its schema for the default.
    Leaf(Value),
    /// A model block: `$defs` body with ordered `properties`.
    Model(Map<String, Value>),
    /// `dict[str, T]`: `additionalProperties`.
    DictOf(Box<Node>),
    /// `list[T]`: `items`.
    ListOf(Box<Node>),
}

fn resolve_schema(schema: &Value, defs: &Map<String, Value>) -> Node {
    // An Optional renders as anyOf [real, null]; the null arm drops.
    if let Some(any_of) = schema.get("anyOf").and_then(|a| a.as_array()) {
        let real: Vec<&Value> = any_of
            .iter()
            .filter(|arm| arm.get("type").and_then(|t| t.as_str()) != Some("null"))
            .collect();
        if real.len() == 1 {
            return resolve_schema(real[0], defs);
        }
    }
    if let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
        let name = reference.trim_start_matches("#/$defs/");
        if let Some(def) = defs.get(name) {
            if let Some(props) = def.get("properties").and_then(|p| p.as_object()) {
                return Node::Model(props.clone());
            }
            return Node::Leaf(def.clone());
        }
        return Node::Leaf(Value::Null);
    }
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            if let Some(additional) = schema.get("additionalProperties") {
                if additional.is_object() {
                    return Node::DictOf(Box::new(resolve_schema(additional, defs)));
                }
            }
            Node::Leaf(Value::Null)
        }
        Some("array") => {
            let items = schema.get("items").cloned().unwrap_or(Value::Null);
            Node::ListOf(Box::new(resolve_schema(&items, defs)))
        }
        _ => Node::Leaf(schema.clone()),
    }
}

fn schema_default(schema: &Value) -> Value {
    schema.get("default").cloned().unwrap_or(Value::Null)
}

/// The value the model serves at this node: raw where a file set it, the
/// schema default where not, models materialized field by field in field
/// order - `model_dump` over the merged tree.
fn resolve_value(node: &Node, raw: Option<&Value>) -> Value {
    match node {
        Node::Leaf(schema) => match raw {
            Some(raw) => raw.clone(),
            None => schema_default(schema),
        },
        Node::ListOf(inner) => match raw {
            Some(Value::Array(items)) => Value::Array(
                items
                    .iter()
                    .map(|item| resolve_value(inner, Some(item)))
                    .collect(),
            ),
            _ => match raw {
                Some(raw) => raw.clone(),
                None => Value::Array(Vec::new()),
            },
        },
        Node::DictOf(inner) => match raw {
            Some(Value::Object(entries)) => {
                let mut out = Map::new();
                for (name, entry) in entries {
                    out.insert(name.clone(), resolve_value(inner, Some(entry)));
                }
                Value::Object(out)
            }
            _ => Value::Object(Map::new()),
        },
        Node::Model(props) => {
            let mut out = Map::new();
            for (field, field_schema) in props {
                let inner = resolve_schema(field_schema, defs());
                let raw_field = raw
                    .and_then(|raw| raw.as_object())
                    .and_then(|map| map.get(field));
                out.insert(field.clone(), resolve_value(&inner, raw_field));
            }
            Value::Object(out)
        }
    }
}

fn defs() -> &'static Map<String, Value> {
    static DEFS: OnceLock<Map<String, Value>> = OnceLock::new();
    DEFS.get_or_init(|| {
        schema_doc()
            .get("json_schema")
            .and_then(|s| s.get("$defs"))
            .and_then(|d| d.as_object())
            .cloned()
            .unwrap_or_default()
    })
}

fn root_props() -> &'static Map<String, Value> {
    static ROOT: OnceLock<Map<String, Value>> = OnceLock::new();
    ROOT.get_or_init(|| {
        schema_doc()
            .get("json_schema")
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default()
    })
}

/// `resolve_dotted` over the schema table: the leading `config.` drops, the
/// `providers` spelling aliases to `accounts`, a `dict[str, Model]` hop with
/// an ABSENT key descends into a default-constructed entry (flipping the
/// `descended` flag so the receipt can name it), and any other miss is
/// unknown.
fn walk(key: &str) -> Result<(Node, Option<Value>, bool), ()> {
    let mut parts: Vec<&str> = key.split('.').collect();
    if parts.first() == Some(&"config") {
        parts.remove(0);
    }
    if parts.first() == Some(&"providers") {
        parts[0] = "accounts";
    }
    let merged = merged_for_cwd();
    let mut raw: Option<Value> = Some(merged);
    let mut node = Node::Model(root_props().clone());
    let mut descended = false;
    for part in parts {
        match node {
            Node::Model(props) => match props.get(part) {
                Some(field_schema) => {
                    node = resolve_schema(field_schema, defs());
                    raw =
                        raw.and_then(|raw| raw.as_object().and_then(|map| map.get(part)).cloned());
                }
                None => return Err(()),
            },
            Node::DictOf(inner) => {
                let entry = raw
                    .as_ref()
                    .and_then(|raw| raw.as_object())
                    .and_then(|map| map.get(part))
                    .cloned();
                match entry {
                    Some(entry) => {
                        node = *inner;
                        raw = Some(entry);
                    }
                    None => {
                        // An absent dict entry reads as the default-constructed
                        // entry; a typo'd name and an unset name land here alike.
                        node = *inner;
                        raw = None;
                        descended = true;
                    }
                }
            }
            _ => return Err(()),
        }
    }
    Ok((node, raw, descended))
}

fn merged_for_cwd() -> Value {
    super::merged_config(&repo_root())
}

// ---------------------------------------------------------------------------
// Rendering, in the shapes the Python leg printed
// ---------------------------------------------------------------------------

/// Python `str()` of a scalar: bools capitalized, floats carrying a decimal,
/// None empty, strings bare.
fn py_scalar(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                int.to_string()
            } else if let Some(int) = number.as_u64() {
                int.to_string()
            } else if let Some(float) = number.as_f64() {
                py_float(float)
            } else {
                number.to_string()
            }
        }
        Value::String(text) => text.clone(),
        other => py_json(other),
    }
}

/// Python float text: `2.0` keeps its decimal, everything else is the
/// shortest round-trip form both runtimes agree on.
fn py_float(float: f64) -> String {
    if float.fract() == 0.0 && float.abs() < 1e16 {
        format!("{float:.1}")
    } else {
        format!("{float}")
    }
}

/// Python `json.dumps` text: `", "` and `": "` separators, `ensure_ascii`
/// escaping, compact enough containers rendered exactly as the old leg's.
pub(super) fn py_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => py_json_string(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_json).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(entries) => {
            let inner: Vec<String> = entries
                .iter()
                .map(|(key, value)| format!("{}: {}", py_json_string(key), py_json(value)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Python `json.dumps` string form with `ensure_ascii=True`.
fn py_json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// The door
// ---------------------------------------------------------------------------

fn get_value(key: &str, json_out: bool) -> i32 {
    let (node, raw, descended) = match walk(key) {
        Ok(answer) => answer,
        Err(()) => {
            eprintln!("error: unknown config key '{key}'");
            return 1;
        }
    };

    let pinned = std::env::var("FNO_CONFIG")
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    let root = repo_root();
    let settings_root: PathBuf = if pinned {
        let pin = PathBuf::from(std::env::var("FNO_CONFIG").unwrap_or_default());
        resolve_path(&pin)
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or(pin.clone())
    } else {
        root.clone()
    };
    // The receipt must describe the chain that PRODUCED the value: a pinned
    // FNO_CONFIG short-circuits the repo chain, so provenance rooted at the
    // repo would name files that did not decide the key.
    let searched = candidate_paths(&root);

    let is_leaf = !matches!(node, Node::Model(_) | Node::DictOf(_));
    let value = resolve_value(&node, raw.as_ref());

    let mut source: Option<(PathBuf, Vec<PathBuf>)> = None;
    if is_leaf {
        source = resolve_source(key, &root);
        if source.is_none() {
            // The value resolved through the rename; provenance must too, or a
            // file that DOES set the key reports "source: default".
            let accounts_key = if let Some(rest) = key.strip_prefix("config.providers.") {
                Some(format!("config.accounts.{rest}"))
            } else if let Some(rest) = key.strip_prefix("providers.") {
                Some(format!("accounts.{rest}"))
            } else if key == "providers" || key == "config.providers" {
                Some("accounts".to_string())
            } else {
                None
            };
            if let Some(accounts_key) = accounts_key {
                source = resolve_source(&accounts_key, &root);
            }
        }
    }

    if json_out {
        let mut envelope = Map::new();
        envelope.insert("key".to_string(), Value::String(key.to_string()));
        envelope.insert("value".to_string(), value.clone());
        envelope.insert(
            "source".to_string(),
            source
                .as_ref()
                .map(|(decider, _)| Value::String(decider.to_string_lossy().to_string()))
                .unwrap_or(Value::Null),
        );
        envelope.insert(
            "overrides".to_string(),
            Value::Array(
                source
                    .as_ref()
                    .map(|(_, overridden)| {
                        overridden
                            .iter()
                            .map(|path| Value::String(path.to_string_lossy().to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
        );
        envelope.insert(
            "root".to_string(),
            Value::String(settings_root.to_string_lossy().to_string()),
        );
        envelope.insert(
            "searched".to_string(),
            Value::Array(
                searched
                    .iter()
                    .map(|path| Value::String(path.to_string_lossy().to_string()))
                    .collect(),
            ),
        );
        println!("{}", py_json(&Value::Object(envelope)));
        return 0;
    }

    match &node {
        Node::Model(_) => println!("{}", serde_json::to_string(&value).unwrap_or_default()),
        Node::DictOf(_) | Node::ListOf(_) => println!("{}", py_json(&value)),
        _ => println!("{}", py_scalar(&value)),
    }

    let searched_list = if searched.is_empty() {
        "<none>".to_string()
    } else {
        searched
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut source_line = if is_leaf {
        match &source {
            Some((decider, _)) => format!("source: {}", decider.to_string_lossy()),
            None => "source: default (no config file sets this key)".to_string(),
        }
    } else {
        "source: mixed - a block merges leaves from several files; query a leaf (e.g. auto_merge.enabled) for its decider".to_string()
    };
    if is_leaf && descended {
        eprintln!("note: no config file sets '{key}'; showing the schema default");
    }
    if is_leaf {
        if let Some((_, overridden)) = &source {
            if !overridden.is_empty() {
                let names: Vec<String> = overridden
                    .iter()
                    .map(|path| path.to_string_lossy().to_string())
                    .collect();
                source_line += &format!(" (overrides {})", names.join(", "));
            }
        }
    }
    source_line += &format!(
        " (root: {}; searched: {})",
        settings_root.to_string_lossy(),
        searched_list
    );
    eprintln!("{source_line}");
    0
}
