//! The fleet-sweep watermark: is the failover host alive, and where is each
//! node in its fallback chain. The Rust owner of
//! `~/.fno/state/fleet-sweep-state.json`, ported from the deleted
//! `cli/src/fno/fleet_state.py` (x-cd91).
//!
//! One file, written as the fleet leg's last act on every daemon tick. A
//! status line is not evidence: a freshness check against this file's mtime
//! answers "did the trigger run" without trusting anyone's self-report.
//!
//! Schema (byte-parity with the Python writer, indent 2 + sorted keys)::
//!
//!     {
//!       "ts": float,                 # epoch of the last completed fleet leg
//!       "candidates": int,           # rows the recovery sweep considered
//!       "refused": int,              # worker_refused events emitted this tick
//!       "silent": int,               # worker_silent events emitted this tick
//!       "chains": {node_id: {"links": [link, ...], "at": float}}
//!     }
//!
//! Not single-writer: the fleet leg runs whether or not the daemon lock is
//! held, so two ticks can overlap on one read-modify-write cycle. Every
//! mutation takes the `<file>.lock` sidecar (flock, 5 s budget) and writes
//! atomically through a temp file plus rename. A contended lock degrades to
//! unlocked: losing the write costs one repeated chain link, blocking the
//! tick costs the failover pass.
//!
//! The door is daemon-free (`fno-agents fleet-state <op>`): a watermark must
//! be writable when the daemon itself is the thing wedged. Python callers
//! (`agents/sweep.py`, `pr_watch/cli.py`) ride it through the one sanctioned
//! crossing, `rust_binary.command_call`.

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

/// A node that has walked its whole chain keeps its row until the node is
/// done. Cap the stored walk so a pathological config cannot grow the file
/// without bound; a chain longer than this is a config problem, not a state
/// problem.
const MAX_LINKS_PER_NODE: usize = 16;

/// How long a node's walk is remembered. Without an age-out, one systematic
/// spawn failure burns the chain link by link and the node then emits
/// `failover_exhausted: all-tried` forever. A day is longer than any measured
/// window and short enough that a stuck node frees itself by the next morning.
const WALK_TTL_S: f64 = 24.0 * 3600.0;

/// Seconds to wait for the sidecar lock. Short on purpose: this is a
/// watermark and a memo, so losing the write costs one repeated chain link,
/// while blocking the tick costs the failover pass.
const LOCK_TIMEOUT_S: f64 = 5.0;

/// The watermark path: Python `paths.state_dir` order (`FNO_STATE_DIR`, then
/// the global config's `[paths] state_dir`, else `$HOME/.fno`), then the
/// state-layout ladder into `state/`. The legacy root spelling renames into
/// `state/` on first resolve and never clobbers an existing `state/` file.
fn fleet_state_path() -> PathBuf {
    let root = state_root();
    let new = root.join("state").join("fleet-sweep-state.json");
    let legacy = root.join("fleet-sweep-state.json");
    if legacy.exists() && !new.exists() {
        if std::fs::create_dir_all(new.parent().unwrap_or(&root)).is_ok() {
            if std::fs::rename(&legacy, &new).is_ok() {
                return new;
            }
        }
        // A racer's winning rename removed legacy; answering legacy would
        // send the write back to the root, so prefer a new that exists.
        return if new.exists() { new } else { legacy };
    }
    new
}

/// The machine state root, mirroring `config.state_dir` for the keys a
/// daemon-free door can read without the Python settings stack: the env pin
/// first, then the global config's TOP-LEVEL `state_dir` (the canonical key;
/// `[paths]` holds per-file overrides only), else `$HOME/.fno`.
fn state_root() -> PathBuf {
    if let Some(root) = std::env::var_os("FNO_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    let ambient = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".fno"))
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let Ok(text) = std::fs::read_to_string(ambient.join("config.toml")) else {
        return ambient;
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return ambient;
    };
    let Some(configured) = doc
        .get("state_dir")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        return ambient;
    };
    let expanded = if configured.starts_with("~/") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(&configured[2..]))
            .unwrap_or_else(|| PathBuf::from(&configured))
    } else {
        PathBuf::from(&configured)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        ambient.join(expanded)
    }
}

/// The stored payload, or an empty object. Fail-open on every read failure:
/// a corrupt watermark must degrade to "no memory" rather than break the
/// tick that would have refreshed it. The cost of an empty read is one
/// repeated chain link; the cost of a raise is the failover trigger not
/// running at all.
fn read_fleet_state(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v @ Value::Object(_)) => v,
            _ => Value::Object(Map::new()),
        },
        Err(_) => Value::Object(Map::new()),
    }
}

/// Serialize with Python `json.dump(..., indent=2, sort_keys=True)` byte
/// parity: every object's keys sorted, two-space indent.
fn canonical_json(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: Map<String, Value> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), sort(v)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect();
                Value::Object(sorted)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    let mut out = serde_json::to_string_pretty(&sort(value)).unwrap_or_else(|_| "{}".into());
    out.push('\n');
    out
}

/// Atomic write through a temp file plus rename in the target's directory.
fn write_payload(path: &Path, payload: &Value) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".fleet-sweep-{}.tmp", std::process::id()));
    {
        let mut fh = std::fs::File::create(&tmp)?;
        fh.write_all(canonical_json(payload).as_bytes())?;
        fh.sync_all().ok();
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The sidecar lock held for a read-modify-write. A contended lock degrades
/// to unlocked after the same 5 s budget the Python leg paid: failing the
/// write outright would cost the whole fleet leg; proceeding unlocked costs
/// at most one lost link, which the next tick re-records.
struct SidecarLock {
    file: Option<std::fs::File>,
    contended: bool,
}

impl SidecarLock {
    fn acquire(path: &Path) -> Self {
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
        else {
            return Self {
                file: None,
                contended: false,
            };
        };
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(LOCK_TIMEOUT_S);
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Self {
                        file: Some(file),
                        contended: false,
                    }
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        eprintln!(
                            "fleet_state: lock contention on {}; writing unlocked",
                            path.display()
                        );
                        return Self {
                            file: Some(file),
                            contended: true,
                        };
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::Error(_)) => {
                    return Self {
                        file: Some(file),
                        contended: true,
                    };
                }
            }
        }
    }
}

/// One node's stored walk, or `[]` when absent, malformed, or aged out.
fn stored_walk(entry: Option<&Value>, now: f64) -> Vec<String> {
    let Some(entry) = entry else {
        return Vec::new();
    };
    let Some(map) = entry.as_object() else {
        return Vec::new();
    };
    let at = map.get("at").and_then(Value::as_f64);
    let Some(at) = at else {
        return Vec::new();
    };
    if now - at > WALK_TTL_S {
        return Vec::new();
    }
    match map.get("links").and_then(Value::as_array) {
        Some(links) => links
            .iter()
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    }
}

fn now_f64(now: Option<f64>) -> f64 {
    now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    })
}

fn op_heartbeat(params: &Params) -> Result<Value, String> {
    let path = params.path()?;
    let candidates = params
        .get("candidates")
        .and_then(Value::as_i64)
        .ok_or("heartbeat needs --candidates N")?;
    let refused = params.get("refused").and_then(Value::as_i64).unwrap_or(0);
    let silent = params.get("silent").and_then(Value::as_i64).unwrap_or(0);
    let now = params.get("now").and_then(Value::as_f64);
    let stamp = now_f64(now);
    let _lock = SidecarLock::acquire(&path);
    let mut payload = read_fleet_state(&path);
    if let Some(map) = payload.as_object_mut() {
        map.insert("ts".into(), json!(stamp));
        map.insert("candidates".into(), json!(candidates));
        map.insert("refused".into(), json!(refused));
        map.insert("silent".into(), json!(silent));
        map.entry("chains".to_string()).or_insert_with(|| json!({}));
    }
    write_payload(&path, &payload).map_err(|e| format!("write failed: {e}"))?;
    Ok(json!({"path": path.display().to_string(), "ts": stamp}))
}

fn op_links(params: &Params) -> Result<Value, String> {
    let path = params.path()?;
    let node = params
        .get("node")
        .and_then(Value::as_str)
        .ok_or("links needs a node id")?
        .to_string();
    let now = now_f64(params.get("now").and_then(Value::as_f64));
    let payload = read_fleet_state(&path);
    let walk = payload
        .get("chains")
        .and_then(Value::as_object)
        .map(|chains| stored_walk(chains.get(&node), now))
        .unwrap_or_default();
    Ok(json!(walk))
}

fn op_record(params: &Params) -> Result<Value, String> {
    let path = params.path()?;
    let node = params
        .get("node")
        .and_then(Value::as_str)
        .ok_or("record needs a node id")?
        .to_string();
    let link = params
        .get("link")
        .and_then(Value::as_str)
        .ok_or("record needs a link")?
        .to_string();
    let stamp = now_f64(params.get("now").and_then(Value::as_f64));
    let _lock = SidecarLock::acquire(&path);
    let mut payload = read_fleet_state(&path);
    let map = payload.as_object_mut().ok_or("payload not an object")?;
    let chains = map.entry("chains".to_string()).or_insert_with(|| json!({}));
    if !chains.is_object() {
        *chains = json!({});
    }
    let chains_map = chains.as_object_mut().unwrap();
    // Written BEFORE the spawn, not after: a link whose spawn dies half-way
    // must still count as tried, or the next tick tries the same failing
    // vendor again. Every write restamps the walk, so the TTL measures time
    // since the last attempt rather than since the first.
    let mut got = stored_walk(chains_map.get(&node), stamp);
    if !got.iter().any(|l| l == &link) {
        got.push(link);
    }
    let kept: Vec<String> = got
        .iter()
        .rev()
        .take(MAX_LINKS_PER_NODE)
        .rev()
        .cloned()
        .collect();
    chains_map.insert(node, json!({"links": &kept, "at": stamp}));
    let answer = json!(kept);
    write_payload(&path, &payload).map_err(|e| format!("write failed: {e}"))?;
    Ok(answer)
}

fn op_clear(params: &Params) -> Result<Value, String> {
    let path = params.path()?;
    let node = params
        .get("node")
        .and_then(Value::as_str)
        .ok_or("clear needs a node id")?
        .to_string();
    let _lock = SidecarLock::acquire(&path);
    let mut payload = read_fleet_state(&path);
    if let Some(chains) = payload.get_mut("chains").and_then(Value::as_object_mut) {
        if chains.remove(&node).is_some() {
            write_payload(&path, &payload).map_err(|e| format!("write failed: {e}"))?;
        }
    }
    Ok(json!({"ok": true}))
}

fn op_silent_seen(params: &Params) -> Result<Value, String> {
    let path = params.path()?;
    let payload = read_fleet_state(&path);
    let handles: Vec<String> = payload
        .get("silent_seen")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // Sorted output: the Python set had no order; a stable wire order keeps
    // the door's output testable without changing what the set means.
    let mut sorted = handles;
    sorted.sort();
    Ok(json!(sorted))
}

fn op_set_silent_seen(params: &Params) -> Result<Value, String> {
    let path = params.path()?;
    let handles: Vec<String> = params
        .get("handles")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .ok_or("set-silent-seen needs handles")?;
    let _lock = SidecarLock::acquire(&path);
    let mut payload = read_fleet_state(&path);
    if let Some(map) = payload.as_object_mut() {
        let mut sorted = handles;
        sorted.sort();
        sorted.dedup();
        map.insert("silent_seen".into(), json!(sorted));
    }
    write_payload(&path, &payload).map_err(|e| format!("write failed: {e}"))?;
    Ok(json!({"ok": true}))
}

fn op_read(params: &Params) -> Result<Value, String> {
    let path = params.path()?;
    Ok(read_fleet_state(&path))
}

/// One parsed invocation: argv flags plus the optional stdin JSON object
/// (`rust_binary.command_call` posts the params there). A stdin key fills a
/// param the argv did not name, so the Python callers speak one shape.
struct Params {
    flags: Map<String, Value>,
    positional: Vec<String>,
    path_override: Option<PathBuf>,
}

impl Params {
    fn get(&self, key: &str) -> Option<&Value> {
        self.flags.get(key)
    }

    fn path(&self) -> Result<PathBuf, String> {
        if let Some(p) = &self.path_override {
            return Ok(p.clone());
        }
        if let Some(p) = self.get("path").and_then(Value::as_str) {
            return Ok(PathBuf::from(p));
        }
        Ok(fleet_state_path())
    }
}

fn parse_args(args: &[String]) -> Result<(String, Params), String> {
    let Some(op) = args.first() else {
        return Err("usage: fno-agents fleet-state <heartbeat|links|record|clear|silent-seen|set-silent-seen|read> [flags]".into());
    };
    let mut flags = Map::new();
    let mut positional = Vec::new();
    let mut path_override: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--path" if i + 1 < args.len() => {
                path_override = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--candidates" | "--refused" | "--silent" | "--now" | "--node" | "--link"
            | "--handles"
                if i + 1 < args.len() =>
            {
                let key = arg.trim_start_matches("--");
                let raw = &args[i + 1];
                let value = match key {
                    "now" => raw
                        .parse::<f64>()
                        .map(|f| json!(f))
                        .unwrap_or_else(|_| json!(raw)),
                    "handles" => json!(raw.split(',').map(str::trim).collect::<Vec<_>>()),
                    "candidates" | "refused" | "silent" => raw
                        .parse::<i64>()
                        .map(|n| json!(n))
                        .unwrap_or_else(|_| json!(raw)),
                    _ => json!(raw),
                };
                flags.insert(key.to_string(), value);
                i += 2;
            }
            other => {
                // An unknown --flag is a usage refusal, never a positional:
                // a typoed flag silently read as a node id would answer an
                // empty walk instead of naming the mistake.
                if other.starts_with("--") {
                    return Err(format!(
                        "unknown fleet-state flag {other:?}; flags: --path, --candidates, --refused, --silent, --now, --node, --link, --handles"
                    ));
                }
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    // Positional fills: `links <node>`, `record <node> <link>`,
    // `set-silent-seen h1,h2` / `set-silent-seen h1 h2`.
    let mut filled = ["", ""];
    for (slot, value) in filled.iter_mut().zip(positional.iter()) {
        *slot = value.as_str();
    }
    if !filled[0].is_empty() && flags.get("node").is_none() && op != "set-silent-seen" {
        flags.insert("node".into(), json!(filled[0]));
    }
    if !filled[1].is_empty() && flags.get("link").is_none() {
        flags.insert("link".into(), json!(filled[1]));
    }
    if op == "set-silent-seen" && flags.get("handles").is_none() && !filled[0].is_empty() {
        let handles: Vec<String> = if positional.len() > 1 {
            positional.clone()
        } else {
            filled[0]
                .split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect()
        };
        flags.insert("handles".into(), json!(handles));
    }
    Ok((
        op.clone(),
        Params {
            flags,
            positional,
            path_override,
        },
    ))
}

/// The door body: `fno-agents fleet-state <op> [flags]`, optional stdin JSON
/// params. Every op answers one JSON object on stdout and exits 0; usage
/// errors exit 2.
pub fn run(args: &[String]) -> i32 {
    let (op, mut params) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    // Merge the stdin JSON object over the argv params: a key stdin names
    // and argv did not fills the param. A human caller on a TTY reads as no
    // params; a piped caller (command_call always pipes) reads one object.
    if !std::io::stdin().is_terminal() {
        if let Ok(text) = std::fs::read_to_string("/dev/stdin") {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text.trim()) {
                for (k, v) in map {
                    if k == "path" && params.path_override.is_none() {
                        if let Some(p) = v.as_str() {
                            params.path_override = Some(PathBuf::from(p));
                        }
                    } else {
                        params.flags.entry(k).or_insert(v);
                    }
                }
            }
        }
    }
    let result = match op.as_str() {
        "heartbeat" => op_heartbeat(&params),
        "links" => op_links(&params),
        "record" => op_record(&params),
        "clear" => op_clear(&params),
        "silent-seen" => op_silent_seen(&params),
        "set-silent-seen" => op_set_silent_seen(&params),
        "read" => op_read(&params),
        other => Err(format!(
            "unknown fleet-state op {other:?}; ops: heartbeat, links, record, clear, silent-seen, set-silent-seen, read"
        )),
    };
    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string(&value).unwrap_or_else(|_| "{}".into())
            );
            0
        }
        Err(msg) => {
            eprintln!("fleet-state: {msg}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(path: &Path, flags: Value) -> Params {
        Params {
            flags: flags.as_object().cloned().unwrap_or_default(),
            positional: Vec::new(),
            path_override: Some(path.to_path_buf()),
        }
    }

    fn temp_path(tag: &str) -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("fno-fs-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("state.json"), dir.display().to_string())
    }

    fn cleanup(dir: &str) {
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_heartbeat_preserves_the_chain_memory() {
        let (path, dir) = temp_path("t1");
        op_record(&params(
            &path,
            json!({"node": "n-1", "link": "codex", "now": 100.0}),
        ))
        .unwrap();
        let out = op_heartbeat(&params(
            &path,
            json!({"candidates": 3, "refused": 1, "silent": 2, "now": 200.0}),
        ))
        .unwrap();
        assert_eq!(out["ts"], json!(200.0));
        let payload = read_fleet_state(&path);
        assert_eq!(payload["candidates"], json!(3));
        assert_eq!(payload["chains"]["n-1"]["links"], json!(["codex"]));
        cleanup(&dir);
    }

    #[test]
    fn a_corrupt_file_reads_as_no_memory() {
        let (path, dir) = temp_path("t2");
        std::fs::write(&path, "not json at all").unwrap();
        let payload = read_fleet_state(&path);
        assert!(payload.as_object().unwrap().is_empty());
        // And the next write recovers the file.
        op_heartbeat(&params(&path, json!({"candidates": 1, "now": 5.0}))).unwrap();
        assert_eq!(read_fleet_state(&path)["candidates"], json!(1));
        cleanup(&dir);
    }

    #[test]
    fn links_accumulate_and_do_not_duplicate() {
        let (path, dir) = temp_path("t3");
        op_record(&params(
            &path,
            json!({"node": "n", "link": "codex", "now": 1.0}),
        ))
        .unwrap();
        op_record(&params(
            &path,
            json!({"node": "n", "link": "claude", "now": 2.0}),
        ))
        .unwrap();
        let links = op_record(&params(
            &path,
            json!({"node": "n", "link": "codex", "now": 3.0}),
        ))
        .unwrap();
        assert_eq!(links, json!(["codex", "claude"]));
        cleanup(&dir);
    }

    #[test]
    fn clear_forgets_one_node_only() {
        let (path, dir) = temp_path("t4");
        op_record(&params(
            &path,
            json!({"node": "a", "link": "x", "now": 1.0}),
        ))
        .unwrap();
        op_record(&params(
            &path,
            json!({"node": "b", "link": "y", "now": 1.0}),
        ))
        .unwrap();
        op_clear(&params(&path, json!({"node": "a"}))).unwrap();
        let payload = read_fleet_state(&path);
        assert!(payload["chains"].get("a").is_none());
        assert_eq!(payload["chains"]["b"]["links"], json!(["y"]));
        cleanup(&dir);
    }

    #[test]
    fn the_stored_walk_is_capped() {
        let (path, dir) = temp_path("t5");
        for i in 0..(MAX_LINKS_PER_NODE as i64 + 4) {
            op_record(&params(
                &path,
                json!({"node": "n", "link": format!("l{i}"), "now": 1.0}),
            ))
            .unwrap();
        }
        let links = op_links(&params(&path, json!({"node": "n", "now": 2.0}))).unwrap();
        assert_eq!(links.as_array().unwrap().len(), MAX_LINKS_PER_NODE);
        cleanup(&dir);
    }

    #[test]
    fn a_walk_older_than_the_ttl_is_forgotten() {
        let (path, dir) = temp_path("t6");
        op_record(&params(
            &path,
            json!({"node": "n", "link": "codex", "now": 0.0}),
        ))
        .unwrap();
        let fresh = op_links(&params(&path, json!({"node": "n", "now": 3600.0}))).unwrap();
        assert_eq!(fresh, json!(["codex"]));
        let aged = op_links(&params(&path, json!({"node": "n", "now": 25.0 * 3600.0}))).unwrap();
        assert_eq!(aged, json!([]));
        cleanup(&dir);
    }

    #[test]
    fn each_attempt_restamps_the_walk() {
        let (path, dir) = temp_path("t7");
        op_record(&params(
            &path,
            json!({"node": "n", "link": "codex", "now": 0.0}),
        ))
        .unwrap();
        // The second attempt at 20h keeps the walk alive past the first
        // stamp's TTL: the TTL measures time since the LAST attempt.
        op_record(&params(
            &path,
            json!({"node": "n", "link": "agy", "now": 20.0 * 3600.0}),
        ))
        .unwrap();
        let links = op_links(&params(&path, json!({"node": "n", "now": 30.0 * 3600.0}))).unwrap();
        assert_eq!(links, json!(["codex", "agy"]));
        cleanup(&dir);
    }

    #[test]
    fn a_handle_that_recovers_re_arms() {
        let (path, dir) = temp_path("t8");
        op_set_silent_seen(&params(&path, json!({"handles": ["w1", "w2"]}))).unwrap();
        let seen = op_silent_seen(&params(&path, json!({}))).unwrap();
        assert_eq!(seen, json!(["w1", "w2"]));
        // Absent handles re-arm by construction: replacing the set drops w2.
        op_set_silent_seen(&params(&path, json!({"handles": ["w1"]}))).unwrap();
        let seen = op_silent_seen(&params(&path, json!({}))).unwrap();
        assert_eq!(seen, json!(["w1"]));
        cleanup(&dir);
    }

    #[test]
    fn the_canonical_writer_matches_the_python_bytes() {
        // Golden captured from the Python leg's json.dump(indent=2,
        // sort_keys=True) before deletion (port protocol).
        let payload: Value = serde_json::from_str(
            r#"{"ts": 1700000000.0, "candidates": 3, "chains": {}, "refused": 1, "silent": 2}"#,
        )
        .unwrap();
        let text = canonical_json(&payload);
        assert_eq!(
            text,
            "{\n  \"candidates\": 3,\n  \"chains\": {},\n  \"refused\": 1,\n  \"silent\": 2,\n  \"ts\": 1700000000.0\n}\n"
        );
    }

    #[test]
    fn an_unknown_flag_refuses_instead_of_becoming_a_node_id() {
        let err = parse_args(&["links".to_string(), "--json".to_string()])
            .err()
            .expect("unknown flag refuses");
        assert!(err.contains("--json"), "{err}");
        // A known flag still parses.
        let (op, params) = parse_args(&[
            "links".to_string(),
            "n-1".to_string(),
            "--now".to_string(),
            "5".to_string(),
        ])
        .unwrap();
        assert_eq!(op, "links");
        assert_eq!(params.get("node").and_then(Value::as_str), Some("n-1"));
        assert_eq!(params.get("now").and_then(Value::as_f64), Some(5.0));
    }
}
