//! The Python string and JSON rules the research output is frozen against:
//! `str.isspace`/`strip`/`split`, `repr`, `json.dumps` with its
//! default separators, and `datetime.isoformat`.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// `str.isspace`: Rust's White_Space set plus the four ASCII separators.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.split()` with no argument: runs of whitespace, empties dropped.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

fn is_printable(c: char) -> bool {
    let u = c as u32;
    if u < 0x20 || u == 0x7f {
        return false;
    }
    if u < 0x7f {
        return true;
    }
    let unprintable = (0x80..=0xa0).contains(&u)
        || u == 0xad
        || u == 0x1680
        || (0x2000..=0x200f).contains(&u)
        || (0x2028..=0x202f).contains(&u)
        || (0x205f..=0x2064).contains(&u)
        || (0x2066..=0x206f).contains(&u)
        || u == 0x3000
        || u == 0xfeff
        || (0xfff9..=0xfffb).contains(&u)
        || (0x0600..=0x0605).contains(&u)
        || u == 0x061c
        || u == 0x06dd
        || u == 0x070f
        || u == 0x180e
        || (0xe000..=0xf8ff).contains(&u)
        || (0xfdd0..=0xfdef).contains(&u)
        || (u & 0xfffe) == 0xfffe
        || (0xe0000..=0xe007f).contains(&u)
        || u >= 0xf0000;
    !unprintable
}

/// `repr(str)`: single quotes unless the text holds a single quote and no
/// double quote, with Python's escapes for the unprintable.
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => {
                let u = c as u32;
                if u <= 0xff {
                    out.push_str(&format!("\\x{u:02x}"));
                } else if u <= 0xffff {
                    out.push_str(&format!("\\u{u:04x}"));
                } else {
                    out.push_str(&format!("\\U{u:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// A JSON string the way `json.dumps(..., ensure_ascii=False)` writes it,
/// except U+0085, U+2028 and U+2029 are escaped: a line-oriented reader
/// splits on them, and a raw one would drop its whole row.
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}') => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `repr(float)`: shortest round-trip digits, exponent form outside
/// `1e-4 <= |x| < 1e16` with a signed two-digit exponent.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let debug = format!("{f:?}");
    match debug.split_once('e') {
        Some((mantissa, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            format!("{mantissa}e{sign}{digits:0>2}")
        }
        None => debug,
    }
}

fn number(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    match n.as_f64() {
        Some(f) => float_repr(f),
        None => n.to_string(),
    }
}

/// `json.dumps(value, ensure_ascii=False)`: `", "` and `": "` separators,
/// keys in their stored order.
pub fn json_dumps(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Number(n) => number(n),
        Value::String(s) => json_string(s),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(json_dumps).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", json_string(k), json_dumps(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python truthiness of a decoded JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(value)` for the scalar shapes a source row can carry.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => number(n),
        Value::String(s) => s.clone(),
        other => json_dumps(other),
    }
}

/// `datetime.isoformat()` for an aware UTC instant: microseconds only when
/// non-zero, `+00:00` offset.
pub fn isoformat(dt: &DateTime<Utc>) -> String {
    let micros = dt.timestamp_subsec_nanos() / 1000;
    let base = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    if micros == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{micros:06}+00:00")
    }
}

/// `isoformat` for a whole-microsecond Unix timestamp (the goldens' clock).
pub fn isoformat_epoch_us(epoch_us: i64) -> String {
    let secs = epoch_us.div_euclid(1_000_000);
    let micros = epoch_us.rem_euclid(1_000_000) as u32;
    match DateTime::from_timestamp(secs, micros * 1000) {
        Some(dt) => isoformat(&dt),
        None => String::new(),
    }
}
