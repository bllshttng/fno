//! The per-install, per-day ceiling on autonomous spawns: the shared
//! `think-spawn-daily.json` counter.
//!
//! Two writers share this file today: Python `fno.provenance.spawn_think`
//! (each `/think` dispatch) and the retro keep-going engine (each `/target`
//! follow-up). Both must agree on its path, its JSON shape, and its write
//! discipline, or one writer resets the other's count. The shape is
//! `{"date": "YYYY-MM-DD", "count": N}` in `json.dumps` spelling, keyed on the
//! LOCAL date, written through a pid-suffixed temp file and an atomic
//! rename, so a reader sees the old file or the new one, never a torn one.
//! The count itself is read-modify-write, last writer wins: a soft ceiling
//! that bounds runaway fan-out, not exact rationing.

use std::path::PathBuf;

use serde_json::Value;

/// `<claims root>/.fno/state/think-spawn-daily.json`, renaming the legacy
/// `<claims root>/.fno/.think-spawn-daily.json` on first resolve.
pub fn counter_path() -> Option<PathBuf> {
    let base = crate::claims_root::global_claims_root()?.join(".fno");
    let new = base.join("state").join("think-spawn-daily.json");
    let legacy = base.join(".think-spawn-daily.json");
    if legacy.exists() && !new.exists() {
        let moved = new
            .parent()
            .is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
            && std::fs::rename(&legacy, &new).is_ok();
        if !moved {
            return Some(legacy);
        }
    }
    Some(new)
}

fn today() -> String {
    chrono::Local::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

/// Today's count; 0 when the file is absent, stale, or unreadable.
pub fn count() -> i64 {
    let Some(path) = counter_path() else { return 0 };
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Ok(obj) = serde_json::from_str::<Value>(&text) else {
        return 0;
    };
    if obj.get("date").and_then(Value::as_str) != Some(today().as_str()) {
        return 0;
    }
    match obj.get("count") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Add one to today's count, resetting on a new day. Best-effort: a failed
/// write never wedges the spawn that called it.
pub fn bump() {
    let Some(path) = counter_path() else { return };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let body = format!("{{\"date\": \"{}\", \"count\": {}}}", today(), count() + 1);
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `think_spawn.daily_cap` from the merged config; 20 when unset or
/// unreadable, and 0 turns the ceiling off.
pub fn daily_cap(project_root: Option<&std::path::Path>) -> i64 {
    let merged = crate::backlog::advance_settings::load_merged(project_root);
    match merged.pointer("/think_spawn/daily_cap") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(20),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(20),
        _ => 20,
    }
}
