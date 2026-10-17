//! The user's keep mark: one marker file per harness session id, read by
//! every row-removing reaper.
//!
//! `fno agents reap --keep <row>` writes `keep/<session id>.json`; the
//! registry sweep, the roster sweep and the merge reaper answer the mark
//! ahead of their policy gates, and the dry run names it
//! (`kept {id} (keep mark: ...)`). The mark is keyed by session id, not by
//! row, so it outlives the row it was set on and follows a `spawn --resume`
//! fork through `predecessor_session_ids` / `forked_from_session_id`.
//! Nothing sweeps a mark on its own: a mark that outlives its row is the
//! design, and `fno agents reap --kept` is how the user sees - and clears -
//! a stale one.
//!
//! Shape copied from `terminal_stop.rs` (session-keyed marker folder,
//! validated ids, pure decision plus thin I/O). No lock, by ruling: every
//! reaper read is one stat and the set is a temp-plus-rename write, so no
//! sweep can block a foreground verb.

use crate::paths::AgentsHome;
use serde::{Deserialize, Serialize};

/// One parsed keep marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepMark {
    pub session_id: String,
    pub name: String,
    pub set_at: String,
    pub set_by: String,
}

/// One row of `--kept`: the parsed marker when the file parses, the file
/// name when it does not. An unreadable marker is listed, never skipped -
/// skipping it would hide the very keep it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedMark {
    pub file: String,
    pub mark: Option<KeepMark>,
}

/// The session-id guard at the marker-write boundary: a filename must stay
/// inside `keep/`. The same shape the lead-manifest cleanup uses (no empty,
/// no separator, no `..`, no NUL).
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
}

/// Whether a handle reads as a full harness session id (hex and dashes, the
/// claude uuid shape: a uuid always carries at least one dash, while a row
/// short id is bare hex, so the dash is what separates the two
/// resolutions). A session id with no registry row is still legal to mark:
/// the roster sweep judges sessions that no row owns.
fn is_session_id(handle: &str) -> bool {
    !handle.is_empty()
        && handle.len() <= 64
        && handle.contains('-')
        && handle.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Write the marker for `session_id`. Refuses an unusable id and writes
/// nothing; otherwise the write is temp-plus-rename, so a reader never sees
/// a partial marker.
pub fn set(home: &AgentsHome, session_id: &str, name: &str) -> std::io::Result<()> {
    if !valid_id(session_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing a keep mark for an unusable session id: {session_id:?}"),
        ));
    }
    let dir = home.keep_dir();
    std::fs::create_dir_all(&dir)?;
    let mark = KeepMark {
        session_id: session_id.to_string(),
        name: name.to_string(),
        set_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        set_by: crate::identity::ambient_agent_handle().unwrap_or_else(|| "human".to_string()),
    };
    let tmp = dir.join(format!("{session_id}.tmp"));
    let path = dir.join(format!("{session_id}.json"));
    std::fs::write(&tmp, serde_json::to_vec(&mark)?)?;
    std::fs::rename(&tmp, &path)
}

/// Remove the marker for `session_id`. `false` when none stood.
pub fn clear(home: &AgentsHome, session_id: &str) -> std::io::Result<bool> {
    if !valid_id(session_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to clear a keep mark for an unusable session id: {session_id:?}"),
        ));
    }
    match std::fs::remove_file(home.keep_dir().join(format!("{session_id}.json"))) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The keep reason one session id carries, or `None` when no mark stands.
/// A failed read never retires a row: an unread or unparseable store reads
/// as kept, with the reason naming what could not be read.
pub fn for_session(home: &AgentsHome, session_id: &str) -> Option<String> {
    if !valid_id(session_id) {
        // An id the store could never mark (set refuses it) carries no mark.
        return None;
    }
    let path = home.keep_dir().join(format!("{session_id}.json"));
    if let Err(e) = std::fs::metadata(&path) {
        if e.kind() == std::io::ErrorKind::NotFound {
            return None;
        }
        return Some(format!("keep mark: store unread ({e})"));
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<KeepMark>(&text) {
            Ok(mark) => Some(format!("keep mark: set {} by {}", mark.set_at, mark.set_by)),
            Err(_) => Some(format!(
                "keep mark: unreadable marker {}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            )),
        },
        Err(e) => Some(format!("keep mark: store unread ({e})")),
    }
}

/// The keep reason one registry row carries: its own session id, any
/// recorded predecessor, or its fork source. An inherited mark names the
/// source id it followed, so the user can find the marker that bit.
pub fn for_row(home: &AgentsHome, e: &crate::state::RegistryEntry) -> Option<String> {
    if let Some(sid) = e.harness_session_id.as_deref() {
        if let Some(why) = for_session(home, sid) {
            return Some(why);
        }
    }
    let short = |sid: &str| sid.chars().take(8).collect::<String>();
    for sid in e.predecessor_session_ids.iter().map(String::as_str) {
        if for_session(home, sid).is_some() {
            return Some(format!("keep mark: inherited from {}", short(sid)));
        }
    }
    if let Some(sid) = e.forked_from_session_id.as_deref() {
        if for_session(home, sid).is_some() {
            return Some(format!("keep mark: inherited from {}", short(sid)));
        }
    }
    None
}

/// Every marker in `keep/`, oldest first. A leftover `.tmp` from a crashed
/// write is not a marker and is skipped; an unparseable `.json` is listed.
pub fn list(home: &AgentsHome) -> Vec<ListedMark> {
    let dir = home.keep_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let mark = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|text| serde_json::from_str::<KeepMark>(&text).ok());
        out.push(ListedMark {
            file: name.to_string(),
            mark: mark.map(|mut m| {
                // The session id on disk is the filename's claim; the listed
                // id is the filename, so a corrupted inner field cannot
                // point the clear command at the wrong marker.
                if m.session_id != stem {
                    m.session_id = stem.to_string();
                }
                m
            }),
        });
    }
    // Parsed markers first, oldest by set_at; ties and the unreadable tail
    // break by file name. One tuple keeps the four orderings in agreement.
    out.sort_by(|a, b| {
        let ka = (
            a.mark.is_none(),
            a.mark.as_ref().map(|m| m.set_at.as_str()).unwrap_or(""),
            a.file.as_str(),
        );
        let kb = (
            b.mark.is_none(),
            b.mark.as_ref().map(|m| m.set_at.as_str()).unwrap_or(""),
            b.file.as_str(),
        );
        ka.cmp(&kb)
    });
    out
}

/// The `--keep` / `--unkeep` / `--kept` arms of `fno agents reap`, parsed
/// beside the sweep the way [`crate::reap_release::from_args`] parses
/// `--release`. Prints and returns the exit code; `None` when no keep flag
/// is present, so the caller falls through to the plain sweep.
pub fn from_args(rest: &[String]) -> Option<i32> {
    let pos = rest
        .iter()
        .position(|a| matches!(a.as_str(), "--keep" | "--unkeep" | "--kept"))?;
    let flag = rest[pos].as_str();
    let takes_handle = flag != "--kept";
    let handle = if takes_handle {
        let handle = rest.get(pos + 1).map(String::as_str).unwrap_or("");
        if handle.is_empty() || handle.starts_with("--") {
            eprintln!("fno-agents: reap {flag} needs a row handle (name, short id or session id)");
            return Some(2);
        }
        handle
    } else {
        ""
    };
    // Everything except the flag, its handle, and --json is an error.
    let extras: Vec<String> = rest
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            *i != pos && !(takes_handle && *i == pos + 1) && !matches!(a.as_str(), "--json" | "-J")
        })
        .map(|(_, a)| a.clone())
        .collect();
    if !extras.is_empty() {
        eprintln!(
            "fno-agents: reap {flag} takes only a handle and --json (got: {})",
            extras.join(" ")
        );
        return Some(2);
    }
    let home = AgentsHome::from_env();
    match flag {
        "--kept" => Some(kept_list(&home)),
        "--keep" => Some(keep_set(&home, handle)),
        _ => Some(keep_unkeep(&home, handle)),
    }
}

fn resolve(home: &AgentsHome, handle: &str) -> Result<(String, String), i32> {
    // The handle resolves by session id first, then by row.
    if is_session_id(handle) {
        let name = crate::reap_release::load_registry_row(home, handle)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| handle.to_string());
        return Ok((handle.to_string(), name));
    }
    match crate::reap_release::load_registry_row(home, handle) {
        Some(row) => match row.harness_session_id.clone().filter(|s| !s.is_empty()) {
            Some(sid) => Ok((sid, row.name.clone())),
            None => {
                eprintln!("{handle} has no harness session id yet; keep it after its first turn");
                Err(2)
            }
        },
        None => {
            eprintln!("{handle} matches no row and is not a session id");
            Err(2)
        }
    }
}

fn keep_set(home: &AgentsHome, handle: &str) -> i32 {
    let (sid, name) = match resolve(home, handle) {
        Ok(found) => found,
        Err(code) => return code,
    };
    if let Err(e) = set(home, &sid, &name) {
        eprintln!("fno-agents: reap --keep refused: {e}");
        return 2;
    }
    let why = for_session(home, &sid).unwrap_or_default();
    println!("kept {name} ({why}); clear with: fno agents reap --unkeep {name}");
    0
}

fn keep_unkeep(home: &AgentsHome, handle: &str) -> i32 {
    let (sid, name) = match resolve(home, handle) {
        Ok(found) => found,
        Err(code) => return code,
    };
    // The row's own marker first; an inherited mark clears at its source,
    // the same lineage for_row reads, so one command lifts the row's keep.
    let mut targets = vec![sid.clone()];
    if let Some(row) = crate::reap_release::load_registry_row(home, &sid) {
        targets.extend(row.predecessor_session_ids.iter().cloned());
        if let Some(fork) = row.forked_from_session_id.clone() {
            targets.push(fork);
        }
    }
    let mut cleared = Vec::new();
    for target in &targets {
        match clear(home, target) {
            Ok(true) => cleared.push(target.clone()),
            Ok(false) => {}
            Err(e) => {
                eprintln!("fno-agents: reap --unkeep refused: {e}");
                return 2;
            }
        }
    }
    if cleared.is_empty() {
        eprintln!("{handle} carries no keep mark");
        return 2;
    }
    println!(
        "unkept {name}; cleared the mark on {}; the row stands or retires on its own gates",
        cleared.join(", ")
    );
    0
}

fn kept_list(home: &AgentsHome) -> i32 {
    let registry = crate::state::load_registry(&home.registry_json()).ok();
    let row_label_of = |sid: &str| {
        registry.as_ref().and_then(|r| {
            r.entries.iter().find(|e| {
                e.harness_session_id.as_deref() == Some(sid) || e.session_id.as_deref() == Some(sid)
            })
        })
    };
    for listed in list(home) {
        let stem = listed.file.strip_suffix(".json").unwrap_or(&listed.file);
        match &listed.mark {
            Some(mark) => {
                let name = &mark.name;
                let set_at = &mark.set_at;
                let set_by = &mark.set_by;
                match row_label_of(stem) {
                    Some(e) => {
                        let label = crate::gc::row_label(e);
                        println!(
                            "{stem} ({name}) set {set_at} by {set_by}; row {label}; clear with: fno agents reap --unkeep {label}"
                        );
                    }
                    None => println!(
                        "{stem} ({name}) set {set_at} by {set_by}; no registry row; clear with: fno agents reap --unkeep {stem}"
                    ),
                }
            }
            None => match row_label_of(stem) {
                Some(e) => {
                    let label = crate::gc::row_label(e);
                    println!(
                        "{stem} (unreadable marker) set ? by ?; row {label}; clear with: remove the marker file {} by hand",
                        listed.file
                    );
                }
                None => println!(
                    "{stem} (unreadable marker) set ? by ?; no registry row; clear with: remove the marker file {} by hand",
                    listed.file
                ),
            },
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RegistryEntry;

    fn temp_home(tag: &str) -> (std::path::PathBuf, AgentsHome) {
        let dir = std::env::temp_dir().join(format!("fno-keep-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), AgentsHome::at(dir.join("home")))
    }

    fn row_with(sid: &str, preds: Vec<String>, fork: Option<String>) -> RegistryEntry {
        let mut e = RegistryEntry::new(
            Some(sid.to_string()),
            crate::state::Lineage::unproven("test"),
        );
        e.harness_session_id = Some(sid.to_string());
        e.predecessor_session_ids = preds;
        e.forked_from_session_id = fork;
        e
    }

    #[test]
    fn set_read_clear_inheritance_unreadable_and_refusals() {
        let (dir, home) = temp_home("store");
        set(&home, "aaaa1111-2222-3333", "w1").unwrap();
        let why = for_session(&home, "aaaa1111-2222-3333").unwrap();
        assert!(why.starts_with("keep mark: set "), "{why}");
        assert!(why.contains(" by "), "{why}");

        // Both inheritance edges read kept and name the source id.
        let mut child = row_with(
            "cccc4444-5555-6666",
            vec!["aaaa1111-2222-3333".into()],
            None,
        );
        let inherited = for_row(&home, &child).unwrap();
        assert_eq!(
            inherited, "keep mark: inherited from aaaa1111",
            "{inherited}"
        );
        child.predecessor_session_ids = Vec::new();
        child.forked_from_session_id = Some("aaaa1111-2222-3333".into());
        let forked = for_row(&home, &child).unwrap();
        assert_eq!(forked, "keep mark: inherited from aaaa1111", "{forked}");

        // The own-id edge names the set time and the setter; clear removes it.
        let own = row_with("aaaa1111-2222-3333", Vec::new(), None);
        assert!(for_row(&home, &own).unwrap().starts_with("keep mark: set "));
        assert!(clear(&home, "aaaa1111-2222-3333").unwrap());
        assert!(for_row(&home, &own).is_none());
        assert!(!clear(&home, "aaaa1111-2222-3333").unwrap());

        // An unreadable marker still keeps and still lists, by file name.
        std::fs::write(
            home.keep_dir().join("bbbb2222-3333-4444.json"),
            b"{not json",
        )
        .unwrap();
        let unreadable = for_session(&home, "bbbb2222-3333-4444").unwrap();
        assert!(
            unreadable.starts_with("keep mark: unreadable marker "),
            "{unreadable}"
        );
        let listed = list(&home);
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(listed[1].mark.is_none(), "{listed:?}");
        assert_eq!(listed[1].file, "bbbb2222-3333-4444.json");

        // An unusable id refuses and writes nothing: no marker can name it.
        for bad in ["", "a/b", "a\\b", "..", "a..b", "x\0y"] {
            assert!(set(&home, bad, "w1").is_err(), "{bad:?}");
        }
        let json_count = std::fs::read_dir(home.keep_dir())
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert_eq!(json_count, 2, "only the two real markers stand");
        assert!(
            for_session(&home, "a/b").is_none(),
            "an unmarkable id carries no mark"
        );

        // The command arms (change 5) over the same home: from_args builds
        // its own AgentsHome, so the env var carries the temp path under the
        // process-global env lock.
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FNO_AGENTS_HOME", &home.root());
        let reg = home.registry_json();
        crate::state::update_registry(&reg, |r| {
            let mut e = RegistryEntry::new(
                Some("dddd5555-6666-7777".to_string()),
                crate::state::Lineage::unproven("test"),
            );
            e.name = "w1".into();
            e.short_id = "dd112233".into();
            e.harness_session_id = Some("dddd5555-6666-7777".into());
            r.entries.push(e);
        })
        .unwrap();

        // Set by name; the receipt names the row, the mark and the clear.
        assert_eq!(
            from_args(&["--keep".to_string(), "w1".to_string()]),
            Some(0)
        );
        assert!(home.keep_dir().join("dddd5555-6666-7777.json").exists());

        // --kept lists the mark under its row.
        assert_eq!(from_args(&["--kept".to_string()]), Some(0));

        // Clear by name, then by session id with no row: every listed mark
        // is clearable.
        assert_eq!(
            from_args(&["--unkeep".to_string(), "w1".to_string()]),
            Some(0)
        );
        assert!(!home.keep_dir().join("dddd5555-6666-7777.json").exists());
        set(&home, "eeee8888-9999-0000", "eeee8888-9999-0000").unwrap();
        assert_eq!(
            from_args(&["--unkeep".to_string(), "eeee8888-9999-0000".to_string()]),
            Some(0)
        );

        // The three refusals, each exit 2.
        assert_eq!(
            from_args(&["--keep".to_string(), "nosuch".to_string()]),
            Some(2)
        );
        assert_eq!(
            from_args(&["--unkeep".to_string(), "w1".to_string()]),
            Some(2),
            "a row with no mark refuses"
        );
        let mut bare = RegistryEntry::new(
            Some("sid-bare".to_string()),
            crate::state::Lineage::unproven("test"),
        );
        bare.name = "bare".into();
        crate::state::update_registry(&reg, |r| r.entries.push(bare)).unwrap();
        assert_eq!(
            from_args(&["--keep".to_string(), "bare".to_string()]),
            Some(2)
        );

        // An inherited mark clears at its source: the row's own id carries
        // no marker, the predecessor does, and --unkeep <row> lifts it.
        let mut child = RegistryEntry::new(
            Some("ffff2222-3333-4444".to_string()),
            crate::state::Lineage::unproven("test"),
        );
        child.name = "w2".into();
        child.harness_session_id = Some("ffff2222-3333-4444".into());
        child.predecessor_session_ids = vec!["eeee8888-9999-0000".into()];
        crate::state::update_registry(&reg, |r| r.entries.push(child)).unwrap();
        set(&home, "eeee8888-9999-0000", "eeee8888-9999-0000").unwrap();
        assert_eq!(
            from_args(&["--unkeep".to_string(), "w2".to_string()]),
            Some(0)
        );
        assert!(!home.keep_dir().join("eeee8888-9999-0000.json").exists());
        let _ = std::env::remove_var("FNO_AGENTS_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
