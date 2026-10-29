//! The unknown-key walker the loader runs before validation:
//! `unknown_key_problems`, `warn_unknown_keys` and `_near_miss_keys` in
//! `fno/config/_readback.py`, with `difflib.get_close_matches` ported for the
//! near-miss hint.

use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// An unknown table with more leaves than this reports as the table.
const UNKNOWN_LEAF_CAP: usize = 3;
/// A leaf name in more sections than this is a common word, not a near miss.
const NEAR_MISS_CAP: usize = 4;
/// Another reader owns `kanban`; `providers` is the pre-rename spelling the
/// loader still aliases across.
const UNMODELED_BLOCKS: [&str; 2] = ["kanban", "providers"];
/// Legacy spellings the loader still lifts, so they never report.
const HONORED_LEGACY_KEYS: [&str; 1] = ["recovery.retire_grace_s"];
/// Keys only the Rust leg reads; they work, so they never report.
const RUST_READ_KEYS: [&str; 1] = ["merge.visual_paint_paths"];
const RETIRED_BLOCKS: [&str; 1] = ["reach_me"];

fn retired_rows(block: &str, path: &std::path::Path) -> String {
    format!(
        "[[{block}]] rows are retired (set in {}): question pages need no setup. Delete the row.",
        path.display()
    )
}

/// Keys the model ignores, each named with the file that holds it. `layers`
/// is the loader's priority-ordered list, each layer already unwrapped.
pub(super) fn unknown_key_problems(layers: &[(PathBuf, Value)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, parsed) in layers {
        let Value::Object(flat) = parsed else {
            continue;
        };
        let modeled: Map<String, Value> = flat
            .iter()
            .filter(|(k, _)| !UNMODELED_BLOCKS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut unknown = Vec::new();
        walk(&modeled, super::ROOT, "", &mut unknown);
        unknown.retain(|k| {
            !HONORED_LEGACY_KEYS.contains(&k.as_str()) && !RUST_READ_KEYS.contains(&k.as_str())
        });
        if flat.get("attention").is_some_and(Value::is_array) {
            problems.push(retired_rows("attention", path));
        }
        for key in unknown {
            let block = key.split('.').next().unwrap_or(key.as_str());
            if RETIRED_BLOCKS.contains(&block) {
                problems.push(retired_rows(block, path));
                continue;
            }
            let hints = near_miss_keys(&key);
            let tail = if hints.is_empty() {
                "ignored".to_string()
            } else {
                format!("did you mean {}?", hints.join(" or "))
            };
            problems.push(format!(
                "{key} (set in {}) is not a modeled config key; {tail}",
                path.display()
            ));
        }
    }
    problems
}

/// `warn_unknown_keys`: dotted keys the model's field set does not carry.
fn walk(data: &Map<String, Value>, model: &str, prefix: &str, out: &mut Vec<String>) {
    let Some(spec) = super::models().get(model) else {
        return;
    };
    let props = super::model_props(model);
    for (key, value) in data {
        let qualified = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if !spec.fields.iter().any(|f| f.name == *key) {
            let leaves: Vec<String> = match value {
                Value::Object(map) if !map.is_empty() => flatten_leaf_paths(map, "")
                    .into_iter()
                    .map(|leaf| format!("{qualified}.{leaf}"))
                    .collect(),
                _ => Vec::new(),
            };
            if !leaves.is_empty() && leaves.len() <= UNKNOWN_LEAF_CAP {
                out.extend(leaves);
            } else {
                out.push(qualified);
            }
            continue;
        }
        let (mapped, item, nested) = props
            .and_then(|p| p.get(key))
            .map(field_models)
            .unwrap_or((None, None, None));
        match (value, item, mapped, nested) {
            (Value::Array(entries), Some(item), _, _) => {
                for (index, entry) in entries.iter().enumerate() {
                    if let Value::Object(entry) = entry {
                        walk(entry, item, &format!("{qualified}[{index}]"), out);
                    }
                }
            }
            (Value::Object(map), _, Some(mapped), _) => {
                for (name, entry) in map {
                    if let Value::Object(entry) = entry {
                        walk(entry, mapped, &format!("{qualified}.{name}"), out);
                    }
                }
            }
            (Value::Object(map), _, None, Some(nested)) => walk(map, nested, &qualified, out),
            _ => {}
        }
    }
}

/// `_flatten_leaf_paths`: the dotted path of every non-dict leaf.
fn flatten_leaf_paths(data: &Map<String, Value>, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in data {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Object(map) => out.extend(flatten_leaf_paths(map, &path)),
            _ => out.push(path),
        }
    }
    out
}

type FieldModels = (
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);

/// `_field_models` read off the JSON schema: `(dict value model, list item
/// model, nested model)`. A union's candidates are its members; any other
/// annotation is itself followed by its type arguments.
fn field_models(node: &'static Value) -> FieldModels {
    let candidates: Vec<&'static Value> = match node.get("anyOf").and_then(Value::as_array) {
        Some(members) => members.iter().collect(),
        None => {
            let mut all = vec![node];
            match node.get("type").and_then(Value::as_str) {
                Some("object") => {
                    all.extend(node.get("additionalProperties").filter(|v| v.is_object()))
                }
                Some("array") => all.extend(node.get("items")),
                _ => all.push(node),
            }
            all
        }
    };
    let mut nested = None;
    for candidate in candidates {
        match candidate.get("type").and_then(Value::as_str) {
            Some("object") => {
                if let Some(model) = candidate.get("additionalProperties").and_then(model_ref) {
                    return (Some(model), None, None);
                }
            }
            Some("array") => {
                if let Some(model) = candidate.get("items").and_then(model_ref) {
                    return (None, Some(model), None);
                }
            }
            _ => {
                if nested.is_none() {
                    nested = model_ref(candidate);
                }
            }
        }
    }
    (None, None, nested)
}

/// A `$ref` to a pydantic model; a dataclass `$ref` is no model.
fn model_ref(node: &Value) -> Option<&'static str> {
    let name = super::ref_name(node)?;
    super::models().get_key_value(name).map(|(k, _)| k.as_str())
}

// ---------------------------------------------------------------------------
// Near-miss hints
// ---------------------------------------------------------------------------

/// `FIELD_META` keys in the registry's own order, which `fields.json`
/// carries: the schema table's leaf rows hold the same set in another
/// order, and a multi-key hint prints in this one.
fn field_meta_keys() -> &'static [String] {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let doc: Value = serde_json::from_str(super::FIELDS).unwrap_or(Value::Null);
        doc["field_meta"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|key| key.as_str().map(str::to_string))
            .collect()
    })
}

fn last_segment(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

/// `_near_miss_keys`: the same leaf in another section first, then a close
/// leaf name; too many hits is a common word and names none.
fn near_miss_keys(unknown: &str) -> Vec<&'static str> {
    let leaf = last_segment(unknown);
    let others: Vec<&'static str> = field_meta_keys()
        .iter()
        .map(String::as_str)
        .filter(|k| *k != unknown)
        .collect();
    let mut hits: Vec<&'static str> = others
        .iter()
        .copied()
        .filter(|k| last_segment(k) == leaf)
        .collect();
    if hits.is_empty() {
        let leaves: Vec<&str> = others.iter().map(|&k| last_segment(k)).collect();
        let close = get_close_matches(leaf, &leaves, 5, 0.85);
        hits = others
            .iter()
            .copied()
            .filter(|k| close.iter().any(|c| c.as_str() == last_segment(k)))
            .collect();
    }
    if hits.len() <= NEAR_MISS_CAP {
        hits
    } else {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// difflib.get_close_matches, SequenceMatcher(None, x, word) with autojunk
// ---------------------------------------------------------------------------

fn get_close_matches(word: &str, possibilities: &[&str], n: usize, cutoff: f64) -> Vec<String> {
    let b: Vec<char> = word.chars().collect();
    let b2j = b2j_of(&b);
    let mut scored: Vec<(f64, &str)> = Vec::new();
    for &x in possibilities {
        let a: Vec<char> = x.chars().collect();
        let length = a.len() + b.len();
        if calculate_ratio(a.len().min(b.len()), length) >= cutoff && quick_ratio(&a, &b) >= cutoff
        {
            let ratio = calculate_ratio(matched(&a, &b, &b2j), length);
            if ratio >= cutoff {
                scored.push((ratio, x));
            }
        }
    }
    // heapq.nlargest over (score, word): score descending, then word descending.
    scored.sort_by(|p, q| {
        q.0.partial_cmp(&p.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| q.1.cmp(p.1))
    });
    scored
        .into_iter()
        .take(n)
        .map(|(_, x)| x.to_string())
        .collect()
}

fn calculate_ratio(matches: usize, length: usize) -> f64 {
    if length > 0 {
        2.0 * matches as f64 / length as f64
    } else {
        1.0
    }
}

/// `b2j` with the autojunk rule: at 200 or more elements, one seen more than
/// `n // 100 + 1` times is popular and leaves the index.
fn b2j_of(b: &[char]) -> HashMap<char, Vec<usize>> {
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (i, ch) in b.iter().enumerate() {
        b2j.entry(*ch).or_default().push(i);
    }
    if b.len() >= 200 {
        let ntest = b.len() / 100 + 1;
        b2j.retain(|_, indices| indices.len() <= ntest);
    }
    b2j
}

fn quick_ratio(a: &[char], b: &[char]) -> f64 {
    let mut full: HashMap<char, i64> = HashMap::new();
    for ch in b {
        *full.entry(*ch).or_insert(0) += 1;
    }
    let mut avail: HashMap<char, i64> = HashMap::new();
    let mut matches = 0;
    for ch in a {
        let numb = match avail.get(ch) {
            Some(left) => *left,
            None => full.get(ch).copied().unwrap_or(0),
        };
        avail.insert(*ch, numb - 1);
        if numb > 0 {
            matches += 1;
        }
    }
    calculate_ratio(matches, a.len() + b.len())
}

/// The summed size of `get_matching_blocks`; the ratio reads nothing else.
fn matched(a: &[char], b: &[char], b2j: &HashMap<char, Vec<usize>>) -> usize {
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut total = 0;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = find_longest_match(a, b, b2j, (alo, ahi, blo, bhi));
        if k > 0 {
            total += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    total
}

/// `find_longest_match`. With `isjunk=None` the junk set is empty, so only the
/// two extension passes over equal non-junk (popular) elements apply.
fn find_longest_match(
    a: &[char],
    b: &[char],
    b2j: &HashMap<char, Vec<usize>>,
    (alo, ahi, blo, bhi): (usize, usize, usize, usize),
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, ch) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut next: HashMap<usize, usize> = HashMap::new();
        for &j in b2j.get(ch).map(Vec::as_slice).unwrap_or(&[]) {
            if j < blo {
                continue;
            }
            if j >= bhi {
                break;
            }
            let k = j
                .checked_sub(1)
                .and_then(|prev| j2len.get(&prev))
                .copied()
                .unwrap_or(0)
                + 1;
            next.insert(j, k);
            if k > bestsize {
                besti = i + 1 - k;
                bestj = j + 1 - k;
                bestsize = k;
            }
        }
        j2len = next;
    }
    while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
        besti -= 1;
        bestj -= 1;
        bestsize += 1;
    }
    while besti + bestsize < ahi
        && bestj + bestsize < bhi
        && a[besti + bestsize] == b[bestj + bestsize]
    {
        bestsize += 1;
    }
    (besti, bestj, bestsize)
}
