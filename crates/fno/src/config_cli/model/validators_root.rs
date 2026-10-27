//! The root config validators, ported from `fno/config/__init__.py`
//! (`ParallelBlock` through `SettingsModel`, plus `_watchdog.lift_retire_grace`
//! and `_tiers.validate_tier_models`). `ConfigBlock`'s validators register
//! under `SettingsModel`, which inherits them.

use super::{py_repr, Ctx, Fail, Registry};
use serde_json::{Map, Value};

const ROOT: &str = "SettingsModel";
const LOOP_LEVELS: [&str; 3] = ["report", "assisted", "unattended"];
const TIER_ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];
const PATH_MAX: usize = 4096;

/// The `ConfigBlock` fields whose before-validator only swaps a non-mapping
/// for `{}`, one decorator each.
const MAPPING_BLOCKS: [&str; 16] = [
    "status_fanout",
    "model_routing",
    "mux",
    "branch",
    "batch",
    "recovery",
    "pr_watch",
    "health_monitor",
    "resource_meter",
    "collision",
    "work",
    "auto_merge",
    "auto_continue",
    "keep_going",
    "think_spawn",
    "active_backlog",
];

pub(super) fn register(reg: &mut Registry) {
    reg.before("ParallelBlock", &["max_lanes"], coerce_max_lanes);
    reg.after("ModelProvider", &["tier_models"], validate_tier_models);
    reg.before(
        "ModelRoutingBlock",
        &["providers", "roles", "extra_env"],
        mapping_or_empty,
    );
    reg.before("LoopEntry", &["level"], coerce_level);
    reg.before("LoopEntry", &["budget_usd_per_run"], coerce_budget);
    reg.before("BatchBlock", &["enabled"], coerce_batch_enabled);
    reg.before("BatchBlock", &["max_nodes"], coerce_max_nodes);
    reg.before("BatchBlock", &["max_loc"], coerce_max_loc);
    reg.after("BranchBlock", &["prefix"], prefix_ref_safe);
    reg.before("MuxBlock", &["shell_integration"], coerce_shell_integration);
    reg.before("AccountsBlock", &["quota"], mapping_or_empty);
    reg.before("AccountsBlock", &["failover"], mapping_or_none);

    reg.model_before(ROOT, lift_legacy_keys);
    reg.before(ROOT, &["status_sinks"], coerce_status_sinks);
    reg.after(ROOT, &["status_sinks"], unique_sink_names);
    for field in MAPPING_BLOCKS {
        reg.before(ROOT, &[field], mapping_or_empty);
    }
    reg.before(ROOT, &["loops"], coerce_loops);
    reg.before(ROOT, &["parallel"], coerce_parallel);
    reg.before(ROOT, &["accounts"], coerce_accounts);
    reg.before(ROOT, &["state_dir", "plans_dir"], validate_path_fields);
    reg.after(ROOT, &["plans_filename"], validate_plans_filename);
    reg.model_after(ROOT, validate_template_references);
    reg.model_before(ROOT, unwrap);
}

/// Python `int(str)`: surrounding whitespace, a sign, and single
/// underscores between digits.
fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if !underscores_ok(body) || !body.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

fn underscores_ok(body: &str) -> bool {
    !body.is_empty() && !body.starts_with('_') && !body.ends_with('_') && !body.contains("__")
}

/// Python `float(str)`.
fn py_float_str(text: &str) -> Option<f64> {
    let t = text.trim();
    let chars: Vec<char> = t.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = i > 0 && chars[i - 1].is_ascii_digit();
            let after = chars.get(i + 1).is_some_and(char::is_ascii_digit);
            if !before || !after {
                return None;
            }
        }
    }
    let cleaned: String = t.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok()
}

fn float_value(f: f64) -> Option<Value> {
    serde_json::Number::from_f64(f).map(Value::Number)
}

fn coerce_max_lanes(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let lanes = match &v {
        Value::Bool(_) => Value::from(1),
        Value::Number(n) if n.is_u64() => v.clone(),
        Value::Number(n) if n.is_i64() => Value::from(n.as_i64().filter(|n| *n >= 0).unwrap_or(1)),
        Value::String(s) => Value::from(py_int(s).filter(|n| *n >= 0).unwrap_or(1)),
        _ => Value::from(1),
    };
    Ok(lanes)
}

fn validate_tier_models(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match v {
        Value::Object(map) if !map.is_empty() => clean_tier_models(&map).map(Value::Object),
        other => Ok(other),
    }
}

fn clean_tier_models(map: &Map<String, Value>) -> Result<Map<String, Value>, Fail> {
    let mut cleaned = Map::new();
    for (key, model) in map {
        let model = match model {
            Value::String(s) => s.trim().to_string(),
            other => super::py_str(other).trim().to_string(),
        };
        cleaned.insert(key.trim().to_lowercase(), Value::String(model));
    }
    let mut bad: Vec<&String> = cleaned
        .keys()
        .filter(|k| !TIER_ALIASES.contains(&k.as_str()))
        .collect();
    bad.sort();
    if !bad.is_empty() {
        return Err(Fail::Value(format!(
            "tier_models keys {} are not Claude tier aliases; legal keys: {}",
            repr_str_list(&bad),
            TIER_ALIASES.join(", ")
        )));
    }
    let mut empty: Vec<&String> = cleaned
        .iter()
        .filter(|(_, m)| m.as_str().unwrap_or("").is_empty())
        .map(|(k, _)| k)
        .collect();
    empty.sort();
    if !empty.is_empty() {
        return Err(Fail::Value(format!(
            "tier_models[{}] names no model",
            repr_str_list(&empty)
        )));
    }
    Ok(cleaned)
}

fn repr_str_list(items: &[&String]) -> String {
    let list: Vec<Value> = items.iter().map(|s| Value::String((*s).clone())).collect();
    py_repr(&Value::Array(list))
}

fn mapping_or_empty(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_object() {
        v
    } else {
        Value::Object(Map::new())
    })
}

fn mapping_or_none(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_object() { v } else { Value::Null })
}

fn coerce_level(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        let level = s.trim().to_lowercase();
        if LOOP_LEVELS.contains(&level.as_str()) {
            return Ok(Value::String(level));
        }
    }
    ctx.note(format!(
        "config.loops.<name>.level={} invalid; using default 'report'",
        py_repr(&v)
    ));
    Ok(Value::String("report".to_string()))
}

fn coerce_budget(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let f = match &v {
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(-1.0),
        Value::String(s) => py_float_str(s).unwrap_or(-1.0),
        _ => -1.0,
    };
    if f <= 0.0 {
        ctx.note(format!(
            "config.loops.<name>.budget_usd_per_run={} invalid; using default 30.0",
            py_repr(&v)
        ));
        return Ok(Value::from(30.0));
    }
    // inf and nan have no JSON number; the raw input goes on to the type check.
    Ok(float_value(f).unwrap_or(v))
}

fn coerce_batch_enabled(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let on = match &v {
        Value::Bool(b) => *b,
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64() == Some(1),
        Value::String(s) => matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    };
    Ok(Value::Bool(on))
}

/// `int(v) >= 1` for an int or str (never a bool), else `None`.
fn positive_int(v: &Value) -> Option<Value> {
    match v {
        Value::Number(n) if n.is_u64() => (n.as_u64() >= Some(1)).then(|| v.clone()),
        Value::Number(n) if n.is_i64() => n.as_i64().filter(|n| *n >= 1).map(Value::from),
        Value::String(s) => py_int(s).filter(|n| *n >= 1).map(Value::from),
        _ => None,
    }
}

fn coerce_max_nodes(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Some(n) = positive_int(&v) {
        return Ok(n);
    }
    ctx.note(format!(
        "config.batch.max_nodes={} invalid; using default 3",
        py_repr(&v)
    ));
    Ok(Value::from(3))
}

fn coerce_max_loc(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if v.is_null() {
        return Ok(v);
    }
    if let Some(n) = positive_int(&v) {
        return Ok(n);
    }
    ctx.note(format!(
        "config.batch.max_loc={} invalid; disabling (None)",
        py_repr(&v)
    ));
    Ok(Value::Null)
}

fn prefix_ref_safe(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let raw = v.as_str().unwrap_or("");
    let prefix = raw.trim().trim_matches('/');
    let unsafe_char = prefix.chars().any(|c| " ~^:?*[\\".contains(c));
    if prefix.is_empty() || prefix.contains("..") || unsafe_char {
        return Err(Fail::Value(
            "config.branch.prefix must be a non-empty git-ref-safe segment".to_string(),
        ));
    }
    Ok(Value::String(prefix.to_string()))
}

fn coerce_shell_integration(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let off = match &v {
        Value::Bool(false) => true,
        Value::String(s) => s.trim() == "off",
        _ => false,
    };
    Ok(Value::String(
        if off { "off" } else { "mux-panes" }.to_string(),
    ))
}

/// `_watchdog.lift_retire_grace`: a legacy `recovery.retire_grace_s` lands
/// on `agents.retire_grace_s` unless the new spelling is already set.
fn lift_legacy_keys(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let lifted = lift_retire_grace(ctx, &v);
    Ok(lifted.unwrap_or(v))
}

fn lift_retire_grace(ctx: &mut Ctx, v: &Value) -> Option<Value> {
    let data = v.as_object()?;
    let recovery = data.get("recovery")?.as_object()?;
    let grace = recovery.get("retire_grace_s")?;
    let mut agents = match data.get("agents") {
        Some(Value::Object(agents)) => agents.clone(),
        _ => Map::new(),
    };
    if !agents.contains_key("retire_grace_s") {
        agents.insert("retire_grace_s".to_string(), grace.clone());
        ctx.note("fno config: recovery.retire_grace_s moved to agents.retire_grace_s; using the legacy value ");
    }
    let recovery: Map<String, Value> = recovery
        .iter()
        .filter(|(k, _)| k.as_str() != "retire_grace_s")
        .map(|(k, x)| (k.clone(), x.clone()))
        .collect();
    let mut out = data.clone();
    out.insert("agents".to_string(), Value::Object(agents));
    out.insert("recovery".to_string(), Value::Object(recovery));
    Some(Value::Object(out))
}

fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn coerce_status_sinks(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if v.is_array() {
        return Ok(v);
    }
    if !v.is_null() {
        ctx.note(format!(
            "config.status_sinks is {}, not an array of tables - ignoring it \
             (use [[status_sinks]], not [status_sinks.<name>])",
            py_type_name(&v)
        ));
    }
    Ok(Value::Array(Vec::new()))
}

fn unique_sink_names(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let names: Vec<Value> = v
        .as_array()
        .into_iter()
        .flatten()
        .map(|sink| sink.get("name").cloned().unwrap_or(Value::Null))
        .collect();
    let mut dups: Vec<Value> = Vec::new();
    for name in &names {
        if names.iter().filter(|n| *n == name).count() > 1 && !dups.contains(name) {
            dups.push(name.clone());
        }
    }
    if dups.is_empty() {
        return Ok(v);
    }
    dups.sort_by(|a, b| super::py_str(a).cmp(&super::py_str(b)));
    Err(Fail::Value(format!(
        "duplicate status sink name(s): {}",
        py_repr(&Value::Array(dups))
    )))
}

fn coerce_loops(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::Object(map) = v else {
        return Ok(Value::Object(Map::new()));
    };
    let mut coerced = Map::new();
    for (name, entry) in map {
        if entry.is_object() {
            coerced.insert(name, entry);
        } else {
            ctx.note(format!(
                "config.loops.{name}={} is not a mapping; dropping (defaults to level 'report')",
                py_repr(&entry)
            ));
        }
    }
    Ok(Value::Object(coerced))
}

fn coerce_parallel(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if v.get("max_lanes").is_some() && v.is_object() {
        ctx.note(
            "fno config: parallel.max_lanes is deprecated and ignored; \
             dispatch width derives from spawn-gate headroom \
             (agents.max_live, agents.provider_limits.<provider>.lanes). \
             Delete the key from config.",
        );
    }
    Ok(if v.is_object() {
        v
    } else {
        Value::Object(Map::new())
    })
}

/// `AccountsBlock(**v)` or, when that refuses, `AccountsBlock()`: an
/// invalid accounts mapping drops to the defaults instead of refusing.
fn coerce_accounts(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if v.is_object() && super::validates_as("AccountsBlock", &v) {
        return Ok(v);
    }
    Ok(Value::Object(Map::new()))
}

fn validate_path_fields(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        let mut found: Vec<String> = ['*', '?', '[']
            .iter()
            .filter(|c| s.contains(**c))
            .map(|c| py_repr(&Value::String(c.to_string())))
            .collect();
        found.sort();
        if !found.is_empty() {
            return Err(Fail::Value(format!(
                "'path field' contains glob character(s) {}; glob characters are not allowed in path values",
                found.join(", ")
            )));
        }
        if s.len() > PATH_MAX {
            return Err(Fail::Value(format!(
                "'path field' exceeds PATH_MAX ({PATH_MAX} bytes): {} bytes",
                s.len()
            )));
        }
    }
    Ok(v)
}

fn validate_plans_filename(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let template = v.as_str().unwrap_or("");
    let rendered = match py_format(&strftime_2000_01_02(template)) {
        Ok(rendered) => rendered,
        Err(e) => {
            return Err(Fail::Value(format!(
                "config.plans_filename: unrenderable template {}: {e} \
                 (strftime codes plus {{slug}} and {{node}} placeholders only)",
                py_repr(&v)
            )))
        }
    };
    if !rendered.ends_with(".md") || rendered.contains('/') {
        return Err(Fail::Value(format!(
            "config.plans_filename: must render to a bare *.md filename, got {}",
            py_repr(&Value::String(rendered))
        )));
    }
    Ok(v)
}

/// `datetime(2000, 1, 2).strftime(fmt)` as CPython on macOS renders it: the
/// Python layer owns `%f`, `%z`, `%Z` and `%:z`; libc drops the `%` of an
/// unknown code and keeps its character.
fn strftime_2000_01_02(fmt: &str) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(&next) = chars.get(i) else {
            out.push('%');
            break;
        };
        match next {
            '%' => {
                out.push('%');
                i += 1;
                continue;
            }
            'f' => {
                out.push_str("000000");
                i += 1;
                continue;
            }
            'z' | 'Z' => {
                i += 1;
                continue;
            }
            ':' if chars.get(i + 1) == Some(&'z') => {
                i += 2;
                continue;
            }
            _ => {}
        }
        let mut flag: Option<char> = None;
        while let Some(&f) = chars.get(i) {
            if f == '-' || f == '_' || f == '0' {
                flag = Some(f);
                i += 1;
            } else {
                break;
            }
        }
        if matches!(chars.get(i), Some('E' | 'O')) && chars.get(i + 1).is_some() {
            i += 1;
        }
        let Some(&conv) = chars.get(i) else { break };
        i += 1;
        let num = |val: i64, width: usize, pad: char| -> String {
            match flag {
                Some('-') => val.to_string(),
                Some('_') => format!("{val:>width$}"),
                Some('0') => format!("{val:0>width$}"),
                _ if pad == '0' => format!("{val:0>width$}"),
                _ => format!("{val:>width$}"),
            }
        };
        let text = match conv {
            'a' => "Sun".to_string(),
            'A' => "Sunday".to_string(),
            'b' | 'h' => "Jan".to_string(),
            'B' => "January".to_string(),
            'c' => "Sun Jan  2 00:00:00 2000".to_string(),
            'C' => num(20, 2, '0'),
            'd' => num(2, 2, '0'),
            'D' | 'x' => "01/02/00".to_string(),
            'e' => num(2, 2, ' '),
            'F' => "2000-01-02".to_string(),
            'G' => num(1999, 4, '0'),
            'g' => num(99, 2, '0'),
            'H' | 'M' | 'S' | 'W' | 'y' => num(0, 2, '0'),
            'I' => num(12, 2, '0'),
            'j' => num(2, 3, '0'),
            'k' => num(0, 2, ' '),
            'l' => num(12, 2, ' '),
            'm' | 'U' => num(1, 2, '0'),
            'n' => "\n".to_string(),
            'p' => "AM".to_string(),
            'R' => "00:00".to_string(),
            'r' => "12:00:00 AM".to_string(),
            's' => local_epoch(),
            'T' | 'X' => "00:00:00".to_string(),
            't' => "\t".to_string(),
            'u' => num(7, 1, '0'),
            'V' => num(52, 2, '0'),
            'v' => " 2-Jan-2000".to_string(),
            'w' => num(0, 1, '0'),
            'Y' => num(2000, 4, '0'),
            '+' => "Sun Jan  2 00:00:00  2000".to_string(),
            other => other.to_string(),
        };
        out.push_str(&text);
    }
    out
}

/// `%s`: the local-time epoch of 2000-01-02 00:00, through libc's TZ rules.
fn local_epoch() -> String {
    // SAFETY: `tm` is plain data, zeroed then filled; mktime only reads and
    // normalizes it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = 100;
    tm.tm_mon = 0;
    tm.tm_mday = 2;
    tm.tm_isdst = -1;
    // SAFETY: `tm` is a valid, exclusively borrowed struct.
    let epoch = unsafe { libc::mktime(&mut tm) };
    epoch.to_string()
}

/// `text.format(slug="s", node="n")`; the error is `str(exc)`.
fn py_format(text: &str) -> Result<String, String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut auto = 0usize;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == '}' {
            if chars.get(i + 1) == Some(&'}') {
                out.push('}');
                i += 2;
                continue;
            }
            return Err("Single '}' encountered in format string".to_string());
        }
        if c != '{' {
            out.push(c);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'{') {
            out.push('{');
            i += 2;
            continue;
        }
        if i + 1 == n {
            return Err("Single '{' encountered in format string".to_string());
        }
        let start = i + 1;
        let mut depth = 1;
        let mut j = start;
        while j < n {
            match chars[j] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if j >= n {
            return Err("expected '}' before end of string".to_string());
        }
        let field: String = chars[start..j].iter().collect();
        out.push_str(&render_field(&field, &mut auto)?);
        i = j + 1;
    }
    Ok(out)
}

fn render_field(field: &str, auto: &mut usize) -> Result<String, String> {
    let split = field.find(['!', ':']).unwrap_or(field.len());
    let (name, rest) = field.split_at(split);
    let (conversion, spec) = match rest.strip_prefix('!') {
        Some(after) => {
            let mut it = after.chars();
            let Some(conv) = it.next() else {
                return Err("end of string while looking for conversion specifier".to_string());
            };
            let tail = it.as_str();
            match tail.strip_prefix(':') {
                Some(spec) => (Some(conv), spec),
                None if tail.is_empty() => (Some(conv), ""),
                None => return Err("expected ':' after conversion specifier".to_string()),
            }
        }
        None => (None, rest.strip_prefix(':').unwrap_or("")),
    };
    let head_end = name.find(['.', '[']).unwrap_or(name.len());
    let (head, mut tail) = name.split_at(head_end);
    if head.is_empty() || head.chars().all(|c| c.is_ascii_digit()) {
        let index = if head.is_empty() {
            *auto
        } else {
            head.parse().unwrap_or(0)
        };
        *auto += 1;
        return Err(format!(
            "Replacement index {index} out of range for positional args tuple"
        ));
    }
    let mut value = match head {
        "slug" => "s".to_string(),
        "node" => "n".to_string(),
        _ => return Err(py_repr(&Value::String(head.to_string()))),
    };
    while !tail.is_empty() {
        if let Some(attr) = tail.strip_prefix('.') {
            let end = attr.find(['.', '[']).unwrap_or(attr.len());
            return Err(format!("'str' object has no attribute '{}'", &attr[..end]));
        }
        let Some(inner) = tail.strip_prefix('[') else {
            return Err("Only '.' or '[' may follow ']' in format field specifier".to_string());
        };
        let Some(close) = inner.find(']') else {
            return Err("Missing ']' in format string".to_string());
        };
        let key = &inner[..close];
        match key.parse::<usize>() {
            Ok(index) => match value.chars().nth(index) {
                Some(ch) => value = ch.to_string(),
                None => return Err("string index out of range".to_string()),
            },
            Err(_) => return Err("string indices must be integers, not 'str'".to_string()),
        }
        tail = &inner[close + 1..];
    }
    let value = match conversion {
        None | Some('s') | Some('a') => value,
        Some('r') => py_repr(&Value::String(value)),
        Some(other) => return Err(format!("Unknown conversion specifier {other}")),
    };
    format_str_spec(&value, spec)
}

/// `format(str, spec)`: fill, align, width, precision and an `s` type.
fn format_str_spec(value: &str, spec: &str) -> Result<String, String> {
    if spec.is_empty() {
        return Ok(value.to_string());
    }
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut fill = ' ';
    let mut align: Option<char> = None;
    if chars.len() >= 2 && "<>^=".contains(chars[1]) {
        fill = chars[0];
        align = Some(chars[1]);
        i = 2;
    } else if "<>^=".contains(chars[0]) {
        align = Some(chars[0]);
        i = 1;
    }
    if chars.get(i).is_some_and(|c| "+- ".contains(*c)) {
        return Err("Sign not allowed in string format specifier".to_string());
    }
    if chars.get(i) == Some(&'#') {
        return Err("Alternate form (#) not allowed in string format specifier".to_string());
    }
    if chars.get(i) == Some(&'0') {
        if align.is_none() {
            fill = '0';
        }
        i += 1;
    }
    let mut width = 0usize;
    while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
        width = width * 10 + d as usize;
        i += 1;
    }
    if let Some(sep) = chars.get(i).filter(|c| **c == ',' || **c == '_') {
        return Err(format!("Cannot specify '{sep}' with 's'."));
    }
    let mut precision: Option<usize> = None;
    if chars.get(i) == Some(&'.') {
        i += 1;
        let mut p = 0usize;
        while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
            p = p * 10 + d as usize;
            i += 1;
        }
        precision = Some(p);
    }
    match &chars[i..] {
        [] | ['s'] => {}
        [code] => {
            return Err(format!(
                "Unknown format code '{code}' for object of type 'str'"
            ))
        }
        _ => {
            return Err(format!(
                "Invalid format specifier '{spec}' for object of type 'str'"
            ))
        }
    }
    if align == Some('=') {
        return Err("'=' alignment not allowed in string format specifier".to_string());
    }
    let text: String = match precision {
        Some(p) => value.chars().take(p).collect(),
        None => value.to_string(),
    };
    let len = text.chars().count();
    if len >= width {
        return Ok(text);
    }
    let pad = width - len;
    let (left, right) = match align {
        Some('>') => (pad, 0),
        Some('^') => (pad / 2, pad - pad / 2),
        _ => (0, pad),
    };
    let fill_n = |count: usize| std::iter::repeat_n(fill, count).collect::<String>();
    Ok(format!("{}{text}{}", fill_n(left), fill_n(right)))
}

/// Rejects `{vault}` in a path while obsidian is off; `{project}` is checked
/// lazily at resolve time.
fn validate_template_references(_ctx: &mut Ctx, m: &mut Map<String, Value>) -> Result<(), Fail> {
    let mut path_values: Vec<(String, String)> = Vec::new();
    for field in ["state_dir", "plans_dir"] {
        if let Some(Value::String(s)) = m.get(field) {
            path_values.push((format!("config.{field}"), s.clone()));
        }
    }
    if let Some(Value::Object(paths)) = m.get("paths") {
        for (name, value) in paths {
            if let Value::String(s) = value {
                path_values.push((format!("config.paths.{name}"), s.clone()));
            }
        }
    }
    let obsidian_on = m
        .get("obsidian")
        .and_then(|o| o.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    for (location, value) in path_values {
        if !obsidian_on && template_vars(&value).iter().any(|var| var == "vault") {
            return Err(Fail::Value(format!(
                "{} uses {{vault}} but obsidian.enabled is false. \
                 Either set obsidian.enabled: true or rewrite the path.",
                py_repr(&Value::String(location))
            )));
        }
    }
    Ok(())
}

/// `re.finditer(r"(?<!\{)\{([^{}]+)\}(?!\})", text)` group 1; the regex
/// crate has no lookaround.
fn template_vars(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut vars = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '{' || (i > 0 && chars[i - 1] == '{') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j] != '{' && chars[j] != '}' {
            j += 1;
        }
        let closed = j > i + 1 && chars.get(j) == Some(&'}') && chars.get(j + 1) != Some(&'}');
        if closed {
            vars.push(chars[i + 1..j].iter().collect());
            i = j + 1;
        } else {
            i += 1;
        }
    }
    vars
}

fn unwrap(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_object() {
        super::super::unwrap_config_dict(&v)
    } else {
        v
    })
}
