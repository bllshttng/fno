//! Eval run history: the append-only JSONL writer (the deleted Python
//! leg's history.py, ported). One newline-terminated compact JSON line per
//! task-run via a single write under O_APPEND|O_CREAT: the kernel
//! serialises concurrent appends at the VFS layer and never interleaves
//! partial writes (POSIX regular-file O_APPEND semantics). The goldens pin
//! the leaf bytes; the row shape is the native fold's input contract.

use std::io::Write;

/// Append *row* as one compact JSON line (created if absent).
pub fn append_row(path: &std::path::Path, row: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // One write carries line + newline: the single O_APPEND write is the
    // concurrent-append atomicity the deleted leg documented.
    let mut line = serde_json::to_string(row).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

/// Append one ATTEMPT row: refuses a row without its unique identity and
/// structured observations - the evidence every verdict classifies from.
pub fn append_attempt(path: &std::path::Path, row: &serde_json::Value) -> Result<(), String> {
    let missing: Vec<&str> = ["attempt_id", "run_id", "obs"]
        .iter()
        .copied()
        .filter(|k| {
            let v = row.get(k);
            match v {
                None => true,
                Some(serde_json::Value::String(s)) => s.is_empty(),
                Some(_) => false,
            }
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "attempt row is missing required identity/evidence: {missing:?}"
        ));
    }
    append_row(path, row)
}
