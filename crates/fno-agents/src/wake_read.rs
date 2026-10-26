//! The wake-signal read, ported from the deleted Python leg `cli/src/fno/
//! wake/signal.py::read_signals`. The only surviving wake surface after the
//! family port: the log-only shell observer `scripts/lib/wake-signals.sh`
//! reads its `kind=question` signals through here instead of the Python
//! wheel. The write path (drop_signal/drain) had no callers and is deleted
//! with the leg, not ported.
//!
//! Hidden op `wake-signals` in the fno-agents client (the same `==` door
//! shape as `board`): `wake-signals --root <repo root> [--kind <kind>]`
//! prints the JSON array `json.dumps(read_signals(root, kind))` produced -
//! Python-default JSON (insertion order, ensure_ascii, ", "/": " spacing),
//! so the shell's `json.load` and the goldens read the same bytes.

use serde_json::{Map, Value};
use std::path::PathBuf;

/// `read_signals`: sorted `wake-*.json` under `<space>/wake-signals/`, parse
/// each, skip unreadable/malformed files, filter by kind, inject `_path`.
pub fn read_signals(root: &std::path::Path, kind: Option<&str>) -> Vec<Value> {
    let dir = crate::paths::space_dir(root).join("wake-signals");
    read_signals_dir(&dir, kind)
}

/// The test seam: the dir form of [`read_signals`], so goldens can pin a
/// sandbox dir directly.
pub fn read_signals_dir(dir: &std::path::Path, kind: Option<&str>) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("wake-") && n.ends_with(".json"))
                && p.is_file()
        })
        .filter_map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)?;
            Some((name, p))
        })
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (_name, path) in names {
        let Ok(text) = signal_text(&path) else {
            continue;
        };
        let Ok(mut payload) = serde_json::from_str::<Value>(&text) else {
            continue;
        }
        // The kind filter is a skip, not an error: a signal of another kind
        // stays on disk for its own reader.
        if let Some(kind) = kind {
            if payload.get("kind").and_then(Value::as_str) != Some(kind) {
                continue;
            }
        }
        if let Some(o) = payload.as_object_mut() {
            o.insert("_path".into(), Value::String(path.display().to_string()));
        }
        out.push(payload);
    }
    out
}

/// Read the payload text (test seam: #[cfg(test)] noted below); an unreadable
/// file skips like a malformed one.
fn signal_text(path: &std::path::Path) -> Result<String, std::io::Error> {
    std::fs::read_to_string(path)
}

/// The op door. Exit 0 with the array; an unparsable --kind is not possible
/// (any string is legal); a bad usage prints and exits 2.
pub fn run_wake_signals(args: &[String]) -> i32 {
    let mut kind: Option<String> = None;
    let mut root: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--kind" => match it.next() {
                Some(k) => kind = Some(k.clone()),
                None => {
                    eprintln!("wake-signals: --kind requires a value");
                    return 2;
                }
            },
            "--root" => match it.next() {
                Some(r) => root = Some(r.clone()),
                None => {
                    eprintln!("wake-signals: --root requires a value");
                    return 2;
                }
            },
            other => {
                eprintln!("wake-signals: unknown flag {other}");
                return 2;
            }
        }
    }
    let root = root.map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        PathBuf::from,
    );
    let signals = read_signals(&root, kind.as_deref());
    println!("{}", python_json_default(&python_list(&signals)));
    0
}

fn python_list(values: &[Value]) -> Value {
    Value::Array(values.to_vec())
}

/// `json.dumps` defaults in Rust: insertion order (serde_json's
/// preserve_order feature keeps Map order), ensure_ascii, and the default
/// `", "` / `": "` separators. Powers the hidden op's stdout, byte-parity
/// with the deleted Python print.
fn python_json_default(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_json_string(s),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(python_json_default).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_json_string(k), python_json_default(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn py_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) <= 0x7E => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_filters_by_kind_and_injects_path() {
        let dir = std::env::temp_dir().join(format!("wake-read-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("wake-a.json"),
            r#"{"ts": "2026-01-01T00:00:00+00:00", "kind": "question", "signal_id": "wake-aaaa"}"#,
        )
        .unwrap();
        fs::write(
            dir.join("wake-b.json"),
            r#"{"kind": "brief", "signal_id": "wake-bbbb"}"#,
        )
        .unwrap();
        fs::write(dir.join("wake-broken.json"), "not json").unwrap();
        fs::write(dir.join("other.json"), r#"{"kind": "question"}"#).unwrap();
        let all = read_signals_dir(&dir, None);
        assert_eq!(all.len(), 2, "malformed + non-wake- names skip");
        assert_eq!(all[0]["signal_id"], "wake-aaaa");
        assert!(all[0]["_path"].as_str().unwrap().contains("wake-a.json"));
        let questions = read_signals_dir(&dir, Some("question"));
        assert_eq!(questions.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_answers_empty() {
        let dir = std::env::temp_dir().join(format!("wake-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(read_signals_dir(&dir, None).is_empty());
    }

    #[test]
    fn output_is_python_default_json() {
        let mut map = Map::new();
        map.insert("kind".into(), Value::String("question".into()));
        map.insert("n".into(), json!(2));
        let s = python_json_default(&Value::Object(map));
        assert_eq!(s, r#"{"kind": "question", "n": 2}"#);
    }
}
