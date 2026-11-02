//! The tick's durable state: the per-PR watermark store and the per-repo
//! branch-provenance cache.
//!
//! A PR with no entry is first-seen: the tick records it without firing, so a
//! corrupt or missing store re-baselines from GitHub instead of mass-firing.
//! Keys are `owner/repo#number`; a bare number is not globally unique.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::pr_park::Paths;

const CACHE_RELPATH: &str = ".fno/branch-provenance.json";

pub(crate) fn make_key(repo_slug: Option<&str>, pr: i64) -> Option<String> {
    let key = format!(
        "{}#{pr}",
        repo_slug.filter(|s| !s.is_empty())?.to_lowercase()
    );
    parse_key(&key).map(|_| key)
}

pub(crate) fn parse_key(key: &str) -> Option<(String, i64)> {
    let (slug, number) = key.split_once('#')?;
    let (owner, repo) = slug.split_once('/')?;
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    let digits = !number.is_empty()
        && !number.starts_with('0')
        && number.chars().all(|c| c.is_ascii_digit());
    if !name_ok(owner) || !name_ok(repo) || !digits {
        return None;
    }
    Some((slug.to_lowercase(), number.parse().ok()?))
}

fn nonnegative(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0)
            .max(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0).max(0),
        Some(Value::Bool(true)) => 1,
        _ => 0,
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

/// Collapse duplicate watermark values without losing dispatched work.
fn merge_entries(
    legacy: &Map<String, Value>,
    canonical: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = legacy.clone();
    merged.extend(canonical.clone());
    let newest = [legacy, canonical]
        .iter()
        .filter_map(|e| e.get("last_review_ts").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .max()
        .map(str::to_string);
    merged.insert("last_review_ts".into(), json!(newest));
    // A terminal observation must not read back OPEN from its duplicate.
    let terminal =
        |v: Option<&Value>| matches!(v.and_then(Value::as_str), Some("MERGED" | "CLOSED"));
    if !terminal(merged.get("last_seen_state")) && terminal(legacy.get("last_seen_state")) {
        merged.insert("last_seen_state".into(), legacy["last_seen_state"].clone());
    }
    merged.insert(
        "merge_dispatched".into(),
        json!(truthy(legacy.get("merge_dispatched")) || truthy(canonical.get("merge_dispatched"))),
    );
    merged.insert(
        "retries".into(),
        json!(nonnegative(legacy.get("retries")).max(nonnegative(canonical.get("retries")))),
    );
    let parked = [canonical.get("parked"), legacy.get("parked")]
        .into_iter()
        .find(|v| truthy(*v))
        .flatten()
        .cloned()
        .unwrap_or(Value::Null);
    merged.insert("parked".into(), parked);
    merged
}

pub(crate) fn default_store_path(cwd: &Path) -> PathBuf {
    Paths::resolve(cwd).state
}

/// Keeps delivery retries apart from the observed PR-state cache.
pub(crate) fn delivery_path(store: &Path) -> PathBuf {
    let stem = store
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("state");
    let ext = store.extension().and_then(|s| s.to_str()).unwrap_or("json");
    store.with_file_name(format!("{stem}-delivery.{ext}"))
}

#[derive(Debug, Default)]
pub(crate) struct Normalization {
    pub normalized: Vec<Value>,
    pub dropped: Vec<Value>,
}

/// Atomic JSON store. Every write takes the flock the Rust pr-park mutations
/// hold and replays this tick's own changes over the file on disk, so a park
/// sweep that rewrote the file during a long merge is merged, never clobbered.
pub(crate) struct Store {
    path: PathBuf,
    data: Map<String, Value>,
    /// This tick's changes since the last persist; `None` removes the key.
    dirty: BTreeMap<String, Option<Value>>,
}

impl Store {
    pub(crate) fn load(path: PathBuf) -> Store {
        let data = read_or_reset(&path);
        Store {
            path,
            data,
            dirty: BTreeMap::new(),
        }
    }

    pub(crate) fn data(&self) -> &Map<String, Value> {
        &self.data
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    /// Upsert in memory; the next persist carries it.
    pub(crate) fn put(&mut self, key: &str, entry: Value) {
        self.data.insert(key.to_string(), entry.clone());
        self.dirty.insert(key.to_string(), Some(entry));
    }

    /// Remove in memory; whether a row was there is the receipt's evidence.
    pub(crate) fn remove(&mut self, key: &str) -> bool {
        let existed = self.data.remove(key).is_some();
        if existed {
            self.dirty.insert(key.to_string(), None);
        }
        existed
    }

    /// Upsert and persist at once: a mid-tick cut must keep the write.
    pub(crate) fn set(&mut self, key: &str, entry: Value) -> Result<(), String> {
        self.put(key, entry);
        self.persist()
    }

    pub(crate) fn persist(&mut self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let lock_path = PathBuf::from(format!("{}.lock", self.path.display()));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&lock_path)
            .map_err(|e| e.to_string())?;
        use std::os::fd::AsRawFd;
        // SAFETY: flock on a file this function owns; closing it releases.
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
        let mut fresh = read_or_reset(&self.path);
        for (key, change) in &self.dirty {
            match change {
                Some(entry) => {
                    fresh.insert(key.clone(), entry.clone());
                }
                None => {
                    fresh.remove(key);
                }
            }
        }
        write_atomic(&self.path, &fresh)?;
        self.data = fresh;
        self.dirty.clear();
        Ok(())
    }

    /// Rewrite legacy keys to qualified keys. A bare number is recoverable
    /// only when exactly one qualified twin exists; anything else is dropped,
    /// because a same-numbered PR in another repo would inherit a parked or
    /// merge_dispatched record and stay silently suppressed forever.
    pub(crate) fn normalize_keys(&mut self) -> Normalization {
        let snapshot: Vec<(String, Value)> = self
            .data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut by_number: BTreeMap<i64, std::collections::BTreeSet<String>> = BTreeMap::new();
        for (raw, _) in &snapshot {
            if let Some((slug, number)) = parse_key(raw) {
                by_number
                    .entry(number)
                    .or_default()
                    .insert(format!("{slug}#{number}"));
            }
        }
        let mut out = Normalization::default();
        let mut rebuilt: Map<String, Value> = Map::new();
        let invalid =
            |key: &str| json!({"key": key, "reason": "invalid-entry", "state": "UNKNOWN"});
        let mut bare = Vec::new();
        for (raw, entry) in &snapshot {
            let Some((slug, number)) = parse_key(raw) else {
                bare.push((raw, entry));
                continue;
            };
            let Some(row) = entry.as_object() else {
                out.dropped.push(invalid(raw));
                continue;
            };
            let canonical = format!("{slug}#{number}");
            match rebuilt.get(&canonical).and_then(Value::as_object) {
                Some(existing) => {
                    let merged = merge_entries(row, existing);
                    rebuilt.insert(canonical.clone(), Value::Object(merged));
                    out.normalized.push(json!({"from": raw, "to": canonical}));
                }
                None => {
                    rebuilt.insert(canonical.clone(), entry.clone());
                    if *raw != canonical {
                        out.normalized.push(json!({"from": raw, "to": canonical}));
                    }
                }
            }
        }
        for (raw, entry) in bare {
            let Some(row) = entry.as_object() else {
                out.dropped.push(invalid(raw));
                continue;
            };
            let number = raw.parse::<i64>().ok().filter(|n| *n > 0);
            let digits = !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit());
            let reason = match number.filter(|_| digits) {
                Some(n) => {
                    let targets = by_number.get(&n);
                    if let Some(only) = targets
                        .filter(|t| t.len() == 1)
                        .and_then(|t| t.iter().next())
                    {
                        let empty = Map::new();
                        let existing = rebuilt
                            .get(only)
                            .and_then(Value::as_object)
                            .unwrap_or(&empty);
                        let merged = merge_entries(row, existing);
                        rebuilt.insert(only.clone(), Value::Object(merged));
                        out.normalized.push(json!({"from": raw, "to": only}));
                        continue;
                    }
                    if targets.is_some_and(|t| t.len() > 1) {
                        "ambiguous-key"
                    } else {
                        "unresolvable-key"
                    }
                }
                None => "invalid-key",
            };
            let state = row
                .get("last_seen_state")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("UNKNOWN");
            out.dropped
                .push(json!({"key": raw, "reason": reason, "state": state}));
        }
        for (raw, _) in &snapshot {
            if !rebuilt.contains_key(raw) {
                self.remove(raw);
            }
        }
        for (key, entry) in rebuilt {
            if self.data.get(&key) != Some(&entry) {
                self.put(&key, entry);
            }
        }
        out
    }
}

fn read_or_reset(path: &Path) -> Map<String, Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Map::new(),
        Err(err) => {
            eprintln!(
                "pr-watch: {} is unreadable ({err}); re-baselining",
                path.display()
            );
            return Map::new();
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => map,
        _ => {
            eprintln!("pr-watch: {} is corrupt; re-baselining", path.display());
            Map::new()
        }
    }
}

fn write_atomic(path: &Path, data: &Map<String, Value>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(data).map_err(|e| e.to_string())? + "\n";
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    // A concurrent sweeper can remove the tmp between write and rename; one
    // rewrite beats aborting the caller's queue walk.
    for attempt in 0..2 {
        let mut tmp = tempfile::Builder::new()
            .prefix(".pr-watcher-state.tmp.")
            .tempfile_in(parent)
            .map_err(|e| e.to_string())?;
        tmp.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        match tmp.persist(path) {
            Ok(_) => return Ok(()),
            Err(err) if attempt == 0 && err.error.kind() == std::io::ErrorKind::NotFound => {
                continue
            }
            Err(err) => return Err(err.error.to_string()),
        }
    }
    Ok(())
}

/// Write the non-CLEAN stranded rows when they differ from the cache. The
/// native graph.md render reads this file; `docs/state-root-inventory.md`
/// holds its contract. A display cache never breaks the tick.
pub(crate) fn write_provenance_cache(repo: &Path, rows: &[Value]) -> bool {
    let out: Vec<Value> = rows
        .iter()
        .filter(|row| row.get("class").and_then(Value::as_str) != Some("CLEAN"))
        .map(|row| {
            let fact = |key: &str| {
                row.get("facts")
                    .and_then(|f| f.get(key))
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let top = |key: &str| row.get(key).cloned().unwrap_or(Value::Null);
            json!({
                "branch": fact("branch"),
                "node": top("node"),
                "klass": top("class"),
                "unpushed": top("unpushed"),
                "has_remote": fact("has_remote"),
                "age": top("age"),
                "pr_number": fact("pr_number"),
                "live": fact("live"),
            })
        })
        .collect();
    let target = repo.join(CACHE_RELPATH);
    let current = std::fs::read_to_string(&target)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<Value>>(&text).ok())
        .unwrap_or_default();
    let current: Vec<Value> = current.into_iter().filter(Value::is_object).collect();
    if out == current {
        return false;
    }
    let written = (|| -> std::io::Result<()> {
        let parent = target.parent().unwrap_or(repo);
        std::fs::create_dir_all(parent)?;
        let mut tmp = tempfile::Builder::new()
            .suffix(".tmp")
            .tempfile_in(parent)?;
        tmp.write_all(serde_json::to_string(&out)?.as_bytes())?;
        tmp.persist(&target).map_err(|e| e.error)?;
        Ok(())
    })();
    if let Err(err) = &written {
        eprintln!(
            "pr-watch: provenance cache write failed for {}: {err}",
            repo.display()
        );
    }
    written.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_needs_a_slug_and_a_positive_number() {
        assert_eq!(make_key(Some("O/R"), 7).as_deref(), Some("o/r#7"));
        assert_eq!(make_key(None, 7), None);
        assert_eq!(parse_key("7"), None);
        assert_eq!(parse_key("o/r#07"), None);
        assert_eq!(parse_key("o/r#7"), Some(("o/r".to_string(), 7)));
    }

    #[test]
    fn a_bare_key_moves_only_onto_a_single_twin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        std::fs::write(
            &path,
            json!({
                "7": {"merge_dispatched": true, "retries": 2, "last_seen_state": "OPEN"},
                "a/b#7": {"retries": 1, "last_seen_state": "OPEN"},
                "9": {"last_seen_state": "OPEN"},
                "a/b#8": {}, "c/d#8": {}, "8": {},
                "junk": {}, "a/b#1": 3
            })
            .to_string(),
        )
        .unwrap();
        let mut store = Store::load(path);
        let n = store.normalize_keys();
        let row = store.get("a/b#7").unwrap();
        assert_eq!(row["merge_dispatched"], json!(true));
        assert_eq!(row["retries"], json!(2));
        assert!(store.get("7").is_none());
        let reason = |key: &str| {
            n.dropped
                .iter()
                .find(|d| d["key"] == json!(key))
                .map(|d| d["reason"].as_str().unwrap().to_string())
        };
        assert_eq!(reason("9").as_deref(), Some("unresolvable-key"));
        assert_eq!(reason("8").as_deref(), Some("ambiguous-key"));
        assert_eq!(reason("junk").as_deref(), Some("invalid-key"));
        assert_eq!(reason("a/b#1").as_deref(), Some("invalid-entry"));
    }

    #[test]
    fn persist_merges_a_concurrent_writer_instead_of_clobbering_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        std::fs::write(&path, json!({"a/b#1": {"parked": null}}).to_string()).unwrap();
        let mut store = Store::load(path.clone());
        store.put("a/b#2", json!({"retries": 1}));
        std::fs::write(
            &path,
            json!({"a/b#1": {"parked": "by-pr-park"}}).to_string(),
        )
        .unwrap();
        store.persist().unwrap();
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disk["a/b#1"]["parked"], json!("by-pr-park"));
        assert_eq!(disk["a/b#2"]["retries"], json!(1));
    }

    #[test]
    fn a_corrupt_store_resets_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        std::fs::write(&path, "{nope").unwrap();
        assert!(Store::load(path).data().is_empty());
    }

    #[test]
    fn the_provenance_cache_drops_clean_and_reports_change_once() {
        let dir = tempfile::tempdir().unwrap();
        let rows = vec![
            json!({"class": "CLEAN", "facts": {"branch": "x"}}),
            json!({"class": "STRANDED", "node": "n", "unpushed": 2, "age": "1d",
                   "facts": {"branch": "b", "has_remote": true, "pr_number": 4, "live": false}}),
        ];
        assert!(write_provenance_cache(dir.path(), &rows));
        assert!(!write_provenance_cache(dir.path(), &rows));
        let text = std::fs::read_to_string(dir.path().join(CACHE_RELPATH)).unwrap();
        let stored: Vec<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0]["klass"], json!("STRANDED"));
        assert_eq!(stored[0]["pr_number"], json!(4));
    }
}
