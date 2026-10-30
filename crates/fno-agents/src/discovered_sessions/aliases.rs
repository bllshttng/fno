//! The friendly-name overlay (`mux/session-names.json`): one stable, unique
//! alias per live session, durable before it is an address.

use super::Candidate;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

const LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const LOCK_POLL: Duration = Duration::from_millis(20);
/// A stored alias longer than this is accretion damage, not a hand edit.
const MAX_STORED_ALIAS_LEN: usize = 80;

/// The retired `<harness>-<hex6+>` form. A harness name can hold a dash, so
/// this matches each name whole rather than splitting at the first dash.
pub fn is_legacy_handle(alias: &str) -> bool {
    crate::provider::KNOWN_HARNESSES.iter().any(|harness| {
        alias
            .strip_prefix(harness)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|hex| hex.len() >= 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    })
}

/// The old disambiguator's damage: a trailing token group repeated back to
/// back, or a length no one types as a mailbox address.
pub fn is_accreted(alias: &str) -> bool {
    if alias.chars().count() > MAX_STORED_ALIAS_LEN {
        return true;
    }
    let parts: Vec<&str> = alias.split('-').collect();
    (1..=parts.len() / 2).any(|k| {
        let end = parts.len();
        parts[end - 2 * k..end - k] == parts[end - k..]
    })
}

pub fn default_alias(project: Option<&str>, short_id: &str) -> String {
    let base = match project.filter(|project| !project.is_empty()) {
        Some(project) => project.rsplit('/').next().unwrap_or(project),
        None => "session",
    };
    let alias = format!("{base}-{short_id}");
    if is_legacy_handle(&alias) {
        format!("project-{alias}")
    } else {
        alias
    }
}

/// Shorten from the head so the session-id tail, the uniqueness carrier, stays.
/// Two chars stay free for a numbered rung, which must not regrow past the cap.
pub fn fit_sid_alias(candidate: &str, sid: &str) -> String {
    let total = candidate.chars().count();
    if total + 2 <= MAX_STORED_ALIAS_LEN {
        return candidate.to_string();
    }
    let sid_len = sid.chars().count();
    let keep = MAX_STORED_ALIAS_LEN.saturating_sub(2 + sid_len + 1);
    let head_len = total.saturating_sub(sid_len + 1);
    let head: String = candidate.chars().take(head_len).take(keep).collect();
    format!("{head}-{sid}")
}

/// Unique aliases within one render. A collision's loser is rebuilt from the
/// full session id, which the map's own key makes unique, and every candidate
/// must survive `is_accreted` so the next pass cannot discard what this emits.
pub fn disambiguate(
    aliases: &BTreeMap<String, String>,
    live: &[Candidate],
) -> BTreeMap<String, String> {
    let mut by_sid: HashMap<&str, &Candidate> = HashMap::new();
    for row in live {
        by_sid.insert(row.session_id.as_str(), row);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (sid, stored) in aliases {
        let mut name = stored.clone();
        if seen.contains(&name) {
            let row = by_sid.get(sid.as_str());
            let project = row.and_then(|row| row.project.as_deref());
            let short = row.map(|row| row.short_id.as_str()).unwrap_or("");
            let mut candidates: Vec<String> = Vec::new();
            if name == default_alias(project, short) {
                candidates.push(fit_sid_alias(&default_alias(project, sid), sid));
            }
            let last = fit_sid_alias(&format!("{name}-{sid}"), sid);
            candidates.push(last.clone());
            let rebuilt = candidates
                .iter()
                .find(|candidate| !seen.contains(candidate) && !is_accreted(candidate));
            name = match rebuilt {
                Some(candidate) => candidate.clone(),
                None => {
                    let mut n = 2;
                    while seen.contains(&format!("{last}-{n}")) {
                        n += 1;
                    }
                    format!("{last}-{n}")
                }
            };
        }
        seen.push(name.clone());
        out.insert(sid.clone(), name);
    }
    out
}

/// The fold under the lock: a row's own legible alias outranks the file, a
/// stored alias survives while its session lives and it is undamaged, and
/// everything else takes the default.
fn assign(
    live: &[Candidate],
    stored: &serde_json::Map<String, Value>,
    row_aliases: &HashMap<String, String>,
) -> BTreeMap<String, String> {
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    for row in live {
        let sid = row.session_id.as_str();
        let kept = stored
            .get(sid)
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty());
        let name = if let Some(own) = row_aliases.get(sid) {
            own.clone()
        } else if let Some(good) = kept.filter(|name| !is_accreted(name) && !is_legacy_handle(name))
        {
            good.to_string()
        } else {
            // Discarding a stored alias strands anything addressed to it, so
            // it is never silent.
            if let Some(damaged) = kept {
                eprintln!(
                    "fno: discarding damaged session alias {}; regenerating from the default",
                    crate::client_commands::py_repr_str(damaged)
                );
            }
            default_alias(row.project.as_deref(), &row.short_id)
        };
        aliases.insert(sid.to_string(), name);
    }
    disambiguate(&aliases, live)
}

/// First legible alias per row, keyed by session. A later row wins a shared key.
fn registry_row_aliases(registry_path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(registry) = crate::state::load_registry(registry_path) else {
        return out;
    };
    for entry in registry.entries {
        let first = entry
            .aliases
            .iter()
            .find(|alias| !alias.is_empty() && !is_accreted(alias) && !is_legacy_handle(alias));
        let sid = entry
            .harness_session_id
            .clone()
            .filter(|sid| !sid.is_empty())
            .unwrap_or_else(|| entry.short_id.clone());
        if let Some(alias) = first.filter(|_| !sid.is_empty()) {
            out.insert(sid, alias.clone());
        }
    }
    out
}

fn acquire_lock(file: &std::fs::File) -> bool {
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        // SAFETY: the fd stays open for the call, and flock touches no memory.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(LOCK_POLL.min(deadline - now));
    }
}

fn write_map(target: &Path, aliases: &BTreeMap<String, String>) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut sorted = serde_json::Map::new();
    for (sid, name) in aliases {
        sorted.insert(sid.clone(), Value::String(name.clone()));
    }
    let body = crate::roles::cli::python_json(&Value::Object(sorted));
    let parent = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.tmp.{}.part", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .and_then(|()| std::fs::rename(&tmp, target));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Assign and persist under an exclusive flock. A failed lock or write returns
/// no aliases: one the send path cannot see in the file is no address. No live
/// sessions rewrites nothing, so one empty scan cannot wipe hand-edited names.
pub fn resolve_aliases(
    live: &[Candidate],
    name_map_path: &Path,
    registry_path: &Path,
) -> Result<BTreeMap<String, String>, String> {
    if live.is_empty() {
        return Ok(BTreeMap::new());
    }
    if let Some(parent) = name_map_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let lock_path = {
        let mut raw = name_map_path.as_os_str().to_os_string();
        raw.push(".lock");
        std::path::PathBuf::from(raw)
    };
    let Ok(lock) = std::fs::File::create(&lock_path) else {
        return Ok(BTreeMap::new());
    };
    if !acquire_lock(&lock) {
        return Ok(BTreeMap::new());
    }
    let stored: serde_json::Map<String, Value> = std::fs::read_to_string(name_map_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    let aliases = assign(live, &stored, &registry_row_aliases(registry_path));
    let unchanged = aliases.len() == stored.len()
        && aliases
            .iter()
            .all(|(sid, name)| stored.get(sid).and_then(Value::as_str) == Some(name.as_str()));
    let persisted = unchanged || write_map(name_map_path, &aliases).is_ok();
    // SAFETY: the fd is open; the unlock releases only this descriptor's hold.
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    Ok(if persisted { aliases } else { BTreeMap::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_write_exposes_no_alias() {
        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join("names.json");
        // A directory at the target makes the rename fail.
        std::fs::create_dir(&map).unwrap();
        let live = vec![Candidate {
            session_id: "abcdef01-0000".to_string(),
            short_id: "abcdef01".to_string(),
            agent: "claude".to_string(),
            ..Candidate::default()
        }];
        let aliases = resolve_aliases(&live, &map, &dir.path().join("none.json")).unwrap();
        assert!(aliases.is_empty());
    }
}
