//! The config walks over the committed JSON schema: keys the model does not
//! carry, and enum values the model refuses. Each runs per layer, so every
//! message names the file that holds the key.
//!
//! The model's field set is the schema's `properties`; a `dict[str, Model]`
//! field is `additionalProperties` naming a model, a `list[Model]` field is
//! `items` naming one, and a nested block is a `$ref`. A map's own keys are
//! data, never schema, so the walk recurses into each VALUE.

use serde_json::Value;

/// An unknown table with more leaves than this reports as the table.
const UNKNOWN_LEAF_CAP: usize = 3;
/// A leaf name in more sections than this is a common word, not a near miss.
const NEAR_MISS_CAP: usize = 4;
/// Schema errors one refusal names before it points at the file for the rest.
const REFUSAL_ERROR_CAP: usize = 5;
/// Blocks another reader owns (`kanban`) or the loader aliases (`providers`).
const UNMODELED_BLOCKS: [&str; 2] = ["kanban", "providers"];
/// Legacy spellings the loader still lifts, and keys only the Rust leg reads.
const HONORED_KEYS: [&str; 2] = ["recovery.retire_grace_s", "merge.visual_paint_paths"];
/// Top-level blocks retired with the md attention arm.
const RETIRED_BLOCKS: [&str; 1] = ["reach_me"];

static NULL: Value = Value::Null;

fn retired_rows(block: &str, path: &str) -> String {
    format!("[[{block}]] rows are retired (set in {path}): question pages need no setup. Delete the row.")
}

/// Follow a `$ref` into `$defs`.
fn deref<'a>(schema: &'a Value, node: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(|r| r.as_str()) {
        Some(reference) => {
            let name = reference.rsplit('/').next().unwrap_or("");
            schema["$defs"].get(name).unwrap_or(&NULL)
        }
        None => node,
    }
}

fn is_model(def: &Value) -> bool {
    def.get("properties")
        .map(|p| p.is_object())
        .unwrap_or(false)
}

enum Shape<'a> {
    Mapped(&'a Value),
    Item(&'a Value),
    Nested(&'a Value),
    Leaf,
}

/// `_field_models`: a field's dict-value model, list-item model, or nested
/// model, the first two winning over the third.
fn field_shape<'a>(schema: &'a Value, field: &'a Value) -> Shape<'a> {
    let candidates: Vec<&Value> = match field.get("anyOf").and_then(|a| a.as_array()) {
        Some(options) => options.iter().collect(),
        None => vec![field],
    };
    let mut nested: Option<&Value> = None;
    for candidate in candidates {
        if candidate.get("type").and_then(|t| t.as_str()) == Some("object") {
            if let Some(values) = candidate
                .get("additionalProperties")
                .filter(|v| v.is_object())
            {
                let def = deref(schema, values);
                if is_model(def) {
                    return Shape::Mapped(def);
                }
            }
        }
        if candidate.get("type").and_then(|t| t.as_str()) == Some("array") {
            if let Some(items) = candidate.get("items") {
                let def = deref(schema, items);
                if is_model(def) {
                    return Shape::Item(def);
                }
            }
        }
        if nested.is_none() && candidate.get("$ref").is_some() {
            let def = deref(schema, candidate);
            if is_model(def) {
                nested = Some(def);
            }
        }
    }
    nested.map(Shape::Nested).unwrap_or(Shape::Leaf)
}

fn root_model(schema: &Value) -> &Value {
    schema
}

/// `warn_unknown_keys`: dotted keys the model's field set does not carry.
fn unknown_in(
    schema: &Value,
    data: &serde_json::Map<String, Value>,
    model: &Value,
    prefix: &str,
) -> Vec<String> {
    let known = &model["properties"];
    let mut unknown = Vec::new();
    for (key, value) in data {
        let qualified = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let Some(field) = known.get(key) else {
            let mut leaves = Vec::new();
            if value.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
                crate::config_cli::flatten_leaf_paths(value, &qualified, &mut leaves);
            }
            if !leaves.is_empty() && leaves.len() <= UNKNOWN_LEAF_CAP {
                unknown.extend(leaves.into_iter().map(|(path, _)| path));
            } else {
                unknown.push(qualified);
            }
            continue;
        };
        match field_shape(schema, field) {
            Shape::Item(item) if value.is_array() => {
                for (index, entry) in value.as_array().into_iter().flatten().enumerate() {
                    if let Some(entry) = entry.as_object() {
                        unknown.extend(unknown_in(
                            schema,
                            entry,
                            item,
                            &format!("{qualified}[{index}]"),
                        ));
                    }
                }
            }
            _ if !value.is_object() => {}
            Shape::Mapped(mapped) => {
                for (name, entry) in value.as_object().into_iter().flatten() {
                    if let Some(entry) = entry.as_object() {
                        unknown.extend(unknown_in(
                            schema,
                            entry,
                            mapped,
                            &format!("{qualified}.{name}"),
                        ));
                    }
                }
            }
            Shape::Nested(nested) => {
                if let Some(map) = value.as_object() {
                    unknown.extend(unknown_in(schema, map, nested, &qualified));
                }
            }
            _ => {}
        }
    }
    unknown
}

/// `unknown_key_problems` over the loader's layers.
pub fn unknown_key_problems(
    schema: &Value,
    leaves: &[String],
    layers: &[(String, Value)],
) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, parsed) in layers {
        let flat = crate::config_cli::unwrap_config_dict(parsed);
        let Some(map) = flat.as_object() else {
            continue;
        };
        let filtered: serde_json::Map<String, Value> = map
            .iter()
            .filter(|(k, _)| !UNMODELED_BLOCKS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let unknown: Vec<String> = unknown_in(schema, &filtered, root_model(schema), "")
            .into_iter()
            .filter(|k| !HONORED_KEYS.contains(&k.as_str()))
            .collect();
        if map.get("attention").map(|a| a.is_array()).unwrap_or(false) {
            problems.push(retired_rows("attention", path));
        }
        for key in unknown {
            let block = key.split('.').next().unwrap_or("");
            if RETIRED_BLOCKS.contains(&block) {
                problems.push(retired_rows(block, path));
                continue;
            }
            let hints = near_miss_keys(&key, leaves);
            let tail = if hints.is_empty() {
                "ignored".to_string()
            } else {
                format!("did you mean {}?", hints.join(" or "))
            };
            problems.push(format!(
                "{key} (set in {path}) is not a modeled config key; {tail}"
            ));
        }
    }
    problems
}

fn leaf_name(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

/// `_near_miss_keys`: the same leaf name in another section first, then a
/// close leaf name; none when the name is too common to mean anything.
fn near_miss_keys(unknown: &str, leaves: &[String]) -> Vec<String> {
    let leaf = leaf_name(unknown);
    let others: Vec<&String> = leaves.iter().filter(|k| k.as_str() != unknown).collect();
    let mut hits: Vec<String> = others
        .iter()
        .filter(|k| leaf_name(k) == leaf)
        .map(|k| k.to_string())
        .collect();
    if hits.is_empty() {
        let names: Vec<&str> = others.iter().map(|k| leaf_name(k)).collect();
        let close = close_matches(leaf, &names, 5, 0.85);
        hits = others
            .iter()
            .filter(|k| close.iter().any(|c| c == leaf_name(k)))
            .map(|k| k.to_string())
            .collect();
    }
    if hits.len() <= NEAR_MISS_CAP {
        hits
    } else {
        Vec::new()
    }
}

/// `difflib.get_close_matches(word, possibilities, n, cutoff)`.
pub(crate) fn close_matches(
    word: &str,
    possibilities: &[&str],
    n: usize,
    cutoff: f64,
) -> Vec<String> {
    let b: Vec<char> = word.chars().collect();
    let mut scored: Vec<(f64, String)> = Vec::new();
    for x in possibilities {
        let a: Vec<char> = x.chars().collect();
        let total = (a.len() + b.len()) as f64;
        if total == 0.0 {
            continue;
        }
        let real_quick = 2.0 * a.len().min(b.len()) as f64 / total;
        if real_quick < cutoff {
            continue;
        }
        let mut counts: std::collections::HashMap<char, i64> = std::collections::HashMap::new();
        for c in &b {
            *counts.entry(*c).or_insert(0) += 1;
        }
        let mut shared = 0usize;
        for c in &a {
            let slot = counts.entry(*c).or_insert(0);
            if *slot > 0 {
                shared += 1;
            }
            *slot -= 1;
        }
        if 2.0 * shared as f64 / total < cutoff {
            continue;
        }
        let ratio = 2.0 * matching_total(&a, &b) as f64 / total;
        if ratio >= cutoff {
            scored.push((ratio, x.to_string()));
        }
    }
    scored.sort_by(|l, r| {
        r.0.partial_cmp(&l.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| r.1.cmp(&l.1))
    });
    scored.into_iter().take(n).map(|(_, x)| x).collect()
}

/// `SequenceMatcher.get_matching_blocks` summed: the longest match (earliest
/// in `a`, then in `b`), then the same on each side of it.
fn matching_total(a: &[char], b: &[char]) -> usize {
    let mut total = 0;
    let mut queue = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, b, alo, ahi, blo, bhi);
        if k == 0 {
            continue;
        }
        total += k;
        if alo < i && blo < j {
            queue.push((alo, i, blo, j));
        }
        if i + k < ahi && j + k < bhi {
            queue.push((i + k, ahi, j + k, bhi));
        }
    }
    total
}

fn longest_match(
    a: &[char],
    b: &[char],
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
    let mut prev = vec![0usize; b.len() + 1];
    for i in alo..ahi {
        let mut row = vec![0usize; b.len() + 1];
        for j in blo..bhi {
            if a[i] == b[j] {
                let k = if j > blo { prev[j - 1] + 1 } else { 1 };
                row[j] = k;
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        prev = row;
    }
    (besti, bestj, bestsize)
}

// ---------------------------------------------------------------------------
// Enum values the schema refuses
// ---------------------------------------------------------------------------

/// A field's legal string set, when the field is one enum (or an optional
/// one). Unions and free strings answer None: their refusals carry shapes
/// this walk does not reproduce, so it reports nothing rather than guess.
fn enum_set(schema: &Value, field: &Value) -> Option<Vec<String>> {
    let strings = |node: &Value| -> Option<Vec<String>> {
        let def = deref(schema, node);
        def.get("enum")?
            .as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    if let Some(options) = field.get("anyOf").and_then(|a| a.as_array()) {
        let real: Vec<&Value> = options
            .iter()
            .filter(|o| o.get("type").and_then(|t| t.as_str()) != Some("null"))
            .collect();
        return match real.as_slice() {
            [only] if real.len() < options.len() => strings(*only),
            _ => None,
        };
    }
    strings(field)
}

/// A leaf field's single declared non-null type, when there is exactly one:
/// the `type` spelling, or the one non-null `anyOf` arm. A union of two or
/// more non-null types gets `None` - the walk draws no verdict on it.
fn declared_leaf_type(field: &Value) -> Option<String> {
    if let Some(t) = field.get("type").and_then(Value::as_str) {
        return match t {
            "null" => None,
            other => Some(other.to_string()),
        };
    }
    if let Some(types) = field.get("type").and_then(Value::as_array) {
        let named: Vec<&str> = types.iter().filter_map(Value::as_str).collect();
        let non_null: Vec<&str> = named.iter().copied().filter(|t| *t != "null").collect();
        return match non_null[..] {
            [one] => Some(one.to_string()),
            _ => None,
        };
    }
    let arms = field.get("anyOf").and_then(Value::as_array)?;
    let typed: Vec<&str> = arms
        .iter()
        .filter_map(|arm| arm.get("type").and_then(Value::as_str))
        .filter(|t| *t != "null")
        .collect();
    match typed[..] {
        [one] => Some(one.to_string()),
        _ => None,
    }
}

/// Whether the field admits a `null`: a `null` type member or a `null`-typed
/// `anyOf` arm.
fn nullable(field: &Value) -> bool {
    if let Some(types) = field.get("type").and_then(Value::as_array) {
        return types.iter().filter_map(Value::as_str).any(|t| t == "null");
    }
    field
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|arms| {
            arms.iter()
                .any(|arm| arm.get("type").and_then(Value::as_str) == Some("null"))
        })
}

/// Whether a value of the declared type admits `value`. Minimal coercion:
/// an `integer` takes any whole number, and that is the whole list - the
/// schema's declared shape is the contract, not pydantic's lax one.
fn leaf_type_admits(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    }
}

/// pydantic's `expected` text: `'a', 'b' or 'c'`.
fn expected_text(legal: &[String]) -> String {
    let quoted: Vec<String> = legal.iter().map(|v| super::py_repr_str(v)).collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        Some((last, _)) => last.clone(),
        None => String::new(),
    }
}

/// One refused value: its loc parts, the input, the legal set.
pub struct Refusal {
    pub loc: Vec<String>,
    pub input: Value,
    pub expected: String,
}

fn refusals_in(
    schema: &Value,
    data: &serde_json::Map<String, Value>,
    model: &Value,
    loc: &[String],
    out: &mut Vec<Refusal>,
) {
    let Some(fields) = model["properties"].as_object() else {
        return;
    };
    for (name, field) in fields {
        let Some(value) = data.get(name) else {
            continue;
        };
        let mut here = loc.to_vec();
        here.push(name.clone());
        if let Some(legal) = enum_set(schema, field) {
            let admitted = match value {
                Value::Null => field.get("anyOf").is_some(),
                Value::String(s) => {
                    legal.contains(s) || legal.contains(&s.trim().to_ascii_lowercase())
                }
                _ => false,
            };
            if !admitted {
                out.push(Refusal {
                    loc: here,
                    input: value.clone(),
                    expected: expected_text(&legal),
                });
            }
            continue;
        }
        match field_shape(schema, field) {
            Shape::Item(item) => {
                for (index, entry) in value.as_array().into_iter().flatten().enumerate() {
                    if let Some(entry) = entry.as_object() {
                        let mut at = here.clone();
                        at.push(index.to_string());
                        refusals_in(schema, entry, item, &at, out);
                    }
                }
            }
            Shape::Mapped(mapped) => {
                for (key, entry) in value.as_object().into_iter().flatten() {
                    if let Some(entry) = entry.as_object() {
                        let mut at = here.clone();
                        at.push(key.clone());
                        refusals_in(schema, entry, mapped, &at, out);
                    }
                }
            }
            Shape::Nested(nested) => {
                if let Some(map) = value.as_object() {
                    refusals_in(schema, map, nested, &here, out);
                }
            }
            Shape::Leaf => {
                // A modeled leaf's declared type, when the schema carries
                // exactly one non-null one: the deleted Python leg's
                // model_validate refused a wrong scalar shape (state_dir = 123
                // cannot be a state dir), so the walk refuses it too. A union
                // of two or more non-null types gets no verdict; null passes
                // only a nullable field.
                if let Some(expected) = declared_leaf_type(field) {
                    let admitted = match value {
                        Value::Null => nullable(field),
                        v => leaf_type_admits(&expected, v),
                    };
                    if !admitted {
                        out.push(Refusal {
                            loc: here,
                            input: value.clone(),
                            expected: format!("of type {expected}"),
                        });
                    }
                }
            }
        }
    }
}

/// The refusals one flat config tree draws.
pub fn refusals(schema: &Value, flat: &Value) -> Vec<Refusal> {
    let mut out = Vec::new();
    if let Some(map) = flat.as_object() {
        refusals_in(schema, map, root_model(schema), &[], &mut out);
    }
    out
}

fn held(node: &Value, loc: &[String]) -> bool {
    let mut cursor = node;
    for part in loc {
        let next = match cursor {
            Value::Object(map) => map.get(part),
            Value::Array(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        };
        match next {
            Some(value) => cursor = value,
            None => return false,
        }
    }
    true
}

/// `describe_config_failure`: one line per refusal naming file, key, value
/// and legal set; the holder is the first layer holding the key.
pub fn describe(refused: &[Refusal], layers: &[(String, Value)]) -> String {
    let mut lines = Vec::new();
    for refusal in refused.iter().take(REFUSAL_ERROR_CAP) {
        let key = refusal.loc.join(".");
        let parent = &refusal.loc[..refusal.loc.len().saturating_sub(1)];
        let holder = layers
            .iter()
            .find(|(_, parsed)| {
                let flat = crate::config_cli::unwrap_config_dict(parsed);
                held(&flat, &refusal.loc) || held(&flat, parent)
            })
            .map(|(path, _)| path.clone())
            .unwrap_or_else(|| "settings".to_string());
        lines.push(format!(
            "{holder}: {key} = {}; {}",
            super::py_repr(&refusal.input),
            refusal.expected
        ));
    }
    if refused.len() > REFUSAL_ERROR_CAP {
        lines.push(format!(
            "... and {} more",
            refused.len() - REFUSAL_ERROR_CAP
        ));
    }
    format!("refusing to load settings:\n  {}", lines.join("\n  "))
}
