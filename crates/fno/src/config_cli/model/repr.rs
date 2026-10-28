//! Python `repr()` and `str()` of a parsed config value, the spellings the
//! refusal text and the validator messages print.

use serde_json::Value;

/// `repr(value)`: strings quoted the way CPython picks quotes, bools and
/// None capitalized, floats always carrying a point or an exponent.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                number.to_string()
            } else {
                py_float(number.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(text) => repr_str(text),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), py_repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `str(value)`: a string bare, everything else as its repr.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// `repr(float)`: the shortest round-trip digits, `.0` on a whole number,
/// and CPython's exponent form at 1e16 and above or below 1e-4.
pub fn py_float(float: f64) -> String {
    if float.is_nan() {
        return "nan".to_string();
    }
    if float.is_infinite() {
        return if float > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    let magnitude = float.abs();
    if magnitude != 0.0 && !(1e-4..1e16).contains(&magnitude) {
        let sci = format!("{float:e}");
        let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp.abs());
    }
    let text = format!("{float}");
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_matches_cpython_spellings() {
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(
            py_repr(&json!({"x": [1, 2.5, null, true]})),
            "{'x': [1, 2.5, None, True]}"
        );
        assert_eq!(py_float(100000.0), "100000.0");
        assert_eq!(py_float(1e16), "1e+16");
        assert_eq!(py_float(0.00001), "1e-05");
    }
}
