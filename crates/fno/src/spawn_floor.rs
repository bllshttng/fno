//! The `--floor` setter on the `fno agents spawn` front door.
//!
//! The user asked for one line that lifts the spawn cap floor: `fno agents
//! spawn --floor N`. Alone it writes `[agents.capacity] floor` to the user
//! config and prints the old and new values; with a name and payload it sets
//! the floor and then spawns. The Rust door peels the flag before any
//! forwarding, so the Python CLI never sees it and the spawn flag ratchet
//! (`cli/src/fno/agents/spawn_flag_owners.py`) never counts it - the cap test
//! below pays the front door's slot against that file's cap instead.
//!
//! The same locked, atomic `[agents]` writer backs the mux settings toggle
//! for `agents.reserve_auto` ([`set_agents_bool`]).

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use toml_edit::{value, DocumentMut, Item, Table};

use crate::digest_overlay::{config_roots, config_str, non_empty_env_os, read_nested_file};

/// The flags this front door owns. `fno agents spawn --floor N` is peeled
/// here, before any forwarding, so the Python Typer command never declares
/// it and `lint_cli`'s Python-only count never sees it.
pub const FRONT_DOOR_SPAWN_FLAGS: &[&str] = &["--floor"];

/// The default the receipt prints when the config carries no floor yet.
/// Pinned equal to the gate's own default by the cap test below, so the
/// printed default cannot drift from the one `capacity` enforces.
pub const GATE_DEFAULT_FLOOR: i64 = 1;

/// A setter refusal and the exit code the front door leaves with. Code 2 is
/// a refusal that wrote nothing; code 1 is a failure after or during a write
/// attempt.
#[derive(Debug)]
pub struct FloorError {
    pub code: i32,
    pub message: String,
}

impl FloorError {
    fn refuse(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FloorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// What `peel` took off the argv, and what is left to forward.
#[derive(Debug)]
pub struct Peeled {
    pub floor: Option<u32>,
    /// The argv with the flag and its value removed. A `--floor` after the
    /// `--` fence stays: past the fence the flags are the harness's.
    pub rest: Vec<String>,
    /// `-h` or `--help` appeared before the fence.
    pub help: bool,
}

/// Take `--floor N` (or `--floor=N`) off an argv that starts `agents spawn`.
/// Scans the tokens after `spawn` up to the first `--` fence. Refusals: a
/// missing value, a value that is not a whole number of 1 or more, and the
/// flag given twice.
pub fn peel(args: &[String]) -> Result<Peeled, String> {
    if args.len() < 2 || args[0] != "agents" || args[1] != "spawn" {
        return Err("expected an `agents spawn` argv".into());
    }
    let mut floor: Option<u32> = None;
    let mut help = false;
    let mut rest: Vec<String> = args[..2].to_vec();
    let mut i = 2;
    while i < args.len() {
        let tok = args[i].as_str();
        if tok == "--" {
            rest.extend_from_slice(&args[i..]);
            break;
        }
        if tok == "-h" || tok == "--help" {
            help = true;
            rest.push(tok.to_string());
            i += 1;
            continue;
        }
        let (flag, inline_value) = match tok.strip_prefix("--floor=") {
            Some(v) => (tok, Some(v.to_string())),
            None if tok == "--floor" => match args.get(i + 1) {
                Some(v) => (tok, Some(v.clone())),
                None => return Err("needs a whole number of 1 or more".into()),
            },
            None => {
                rest.push(tok.to_string());
                i += 1;
                continue;
            }
        };
        if floor.is_some() {
            return Err(format!("{flag} given twice"));
        }
        let raw = inline_value.expect("set above for both spellings");
        let parsed = raw.parse::<u32>().ok().filter(|v| *v >= 1);
        match parsed {
            Some(v) => floor = Some(v),
            None => return Err(format!("{raw} is not a whole number of 1 or more")),
        }
        i += if tok == "--floor" { 2 } else { 1 };
    }
    Ok(Peeled { floor, rest, help })
}

/// The receipt the setter prints: the old value, the new one, the file it
/// landed in, and - only when the value sits above `agents.max_live` - the
/// clamp line, because `capacity` holds a floor above the ceiling at it.
#[derive(Debug)]
pub struct FloorReceipt {
    pub old: Option<i64>,
    pub new: u32,
    pub target: PathBuf,
    pub unchanged: bool,
    pub max_live: Option<usize>,
}

impl std::fmt::Display for FloorReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let old = match self.old {
            Some(o) => o.to_string(),
            None => format!("{GATE_DEFAULT_FLOOR} (default)"),
        };
        let tail = if self.unchanged { " (unchanged)" } else { "" };
        write!(
            f,
            "agents.capacity.floor: {old} -> {} in {}{tail}",
            self.new,
            self.target.display()
        )?;
        if let Some(ceiling) = self.max_live {
            if usize::try_from(self.new).map_or(false, |n| n > ceiling) {
                write!(
                    f,
                    "\nagents.max_live is {ceiling}, so the floor holds the cap at {ceiling}. \
                     Past it, a spawn admits only while CPU and RAM read room. \
                     To raise it: fno config set agents.max_live N"
                )?;
            }
        }
        Ok(())
    }
}

/// Resolve the write target and the shadowing files, then set the floor.
/// `$FNO_CONFIG` is the sole target when set (the gate reads only that file
/// then); otherwise the target is the user global config and the shadows are
/// the project ladder the gate walks first.
pub fn set_floor(floor: u32, cwd: &Path) -> Result<FloorReceipt, FloorError> {
    let (target, shadows) = write_target(cwd)?;
    set_floor_at(floor, &target, &shadows, cwd)
}

/// The config.toml an `[agents]` setter writes, and the files that shadow
/// it: `$FNO_CONFIG` alone when set (fno-agents reads only that file then),
/// else the user global config behind the project ladder.
fn write_target(cwd: &Path) -> Result<(PathBuf, Vec<PathBuf>), FloorError> {
    let pinned = non_empty_env_os("FNO_CONFIG")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    if let Some(path) = pinned {
        return Ok((path, Vec::new()));
    }
    let target = gate_global_config().ok_or_else(|| {
        FloorError::refuse(1, "no user config.toml to write (set FNO_CONFIG or HOME)")
    })?;
    // fno-agents reads the project ladder before the global file, so a value
    // set there wins and writing the global would be a no-op the receipt
    // lies about. Anchored on cwd, like the fno-agents reader.
    let mut shadows: Vec<PathBuf> = Vec::new();
    let push = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    for root in config_roots(cwd) {
        push(root.join(".fno/config.toml"), &mut shadows);
    }
    push(cwd.join(".fno/config.toml"), &mut shadows);
    shadows.retain(|p| p != &target);
    Ok((target, shadows))
}

/// Write `[agents] <key> = value` to the config fno-agents reads for `cwd`
/// (the mux's settings toggle for `agents.reserve_auto`), refusing when a
/// project config already sets the key. Returns the file written.
pub fn set_agents_bool(key: &str, on: bool, cwd: &Path) -> Result<PathBuf, FloorError> {
    let (target, shadows) = write_target(cwd)?;
    if let Some(shadow) = shadows
        .iter()
        .find(|s| config_str_at(s, "agents", key).is_some())
    {
        return Err(FloorError::refuse(
            2,
            format!(
                "{} sets agents.{key}, and fno-agents reads it first. Change it there.",
                shadow.display()
            ),
        ));
    }
    let (target, _lock, mut doc) = open_doc(&target)?;
    let agents = ensure_table(
        doc.as_table_mut().entry("agents").or_insert(Item::None),
        "[agents]",
        &target,
    )?;
    agents[key] = value(on);
    commit_doc(&target, &doc)?;
    Ok(target)
}

/// One `[section] key` from one config file, unparsed.
fn config_str_at(path: &Path, section: &str, key: &str) -> Option<toml::Value> {
    let parsed: toml::Value = fs::read_to_string(path).ok()?.parse().ok()?;
    parsed.get(section)?.get(key).cloned()
}

/// Resolve a symlinked target, create its directory, take the config lock,
/// and parse it (a missing file is an empty config). The lock rides the
/// returned tuple, so it holds until the caller's write lands.
fn open_doc(target: &Path) -> Result<(PathBuf, ConfigLock, DocumentMut), FloorError> {
    let mut target = target.to_path_buf();
    if target.is_symlink() {
        target = fs::canonicalize(&target).map_err(|e| {
            FloorError::refuse(1, format!("{} cannot be resolved: {e}", target.display()))
        })?;
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| {
        FloorError::refuse(1, format!("{} cannot be created: {e}", parent.display()))
    })?;
    let lock = ConfigLock::acquire(&target)?;
    let text = match fs::read_to_string(&target) {
        Ok(text) => text,
        // A missing target is an empty config. Any other read failure is a
        // refusal, because writing would replace a config we never saw.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(FloorError::refuse(
                1,
                format!("{} cannot be read: {e}", target.display()),
            ))
        }
    };
    let doc: DocumentMut = text.parse().map_err(|e| {
        FloorError::refuse(
            2,
            format!(
                "{} is not valid TOML ({e}); nothing written.",
                target.display()
            ),
        )
    })?;
    Ok((target, lock, doc))
}

/// Write `doc` over `target`. A missing target gets 0600; an existing one
/// keeps its mode.
fn commit_doc(target: &Path, doc: &DocumentMut) -> Result<(), FloorError> {
    let mode = fs::metadata(target)
        .map(|m| m.permissions())
        .unwrap_or_else(|_| fs::Permissions::from_mode(0o600));
    write_atomic(target, &doc.to_string(), mode)
}

/// Set the floor in `target`, refusing when any `shadows` path already sets
/// a positive floor (the gate reads those first, so a global write would not
/// take). A non-positive or non-integer shadow value is unset to the gate,
/// so it is unset here too. Tests call this with temp paths and no env.
pub fn set_floor_at(
    floor: u32,
    target: &Path,
    shadows: &[PathBuf],
    cwd: &Path,
) -> Result<FloorReceipt, FloorError> {
    for shadow in shadows {
        if let Some(text) = read_nested_file(shadow, "agents", "capacity", "floor") {
            if let Ok(k) = text.trim().parse::<i64>() {
                if k > 0 {
                    return Err(FloorError::refuse(
                        2,
                        format!(
                            "{} sets agents.capacity.floor = {k}, and the gate reads it before \
                             {}. Change it there, or remove the key.",
                            shadow.display(),
                            target.display()
                        ),
                    ));
                }
            }
        }
    }
    let (target, _lock, mut doc) = open_doc(target)?;
    let old = read_floor_int(&doc);
    if old == Some(i64::from(floor)) {
        return Ok(FloorReceipt {
            old,
            new: floor,
            target,
            unchanged: true,
            max_live: max_live(cwd),
        });
    }
    let agents = ensure_table(
        doc.as_table_mut().entry("agents").or_insert(Item::None),
        "[agents]",
        &target,
    )?;
    let capacity = ensure_table(
        agents.entry("capacity").or_insert(Item::None),
        "[agents.capacity]",
        &target,
    )?;
    capacity["floor"] = value(i64::from(floor));
    commit_doc(&target, &doc)?;
    // Readback through the plain reader, not the editor: the write is only
    // done when what landed on disk parses back to the value.
    let written = fs::read_to_string(&target).map_err(|e| {
        FloorError::refuse(1, format!("{} cannot be read back: {e}", target.display()))
    })?;
    let parsed: toml::Value = written.parse().map_err(|e| {
        FloorError::refuse(1, format!("{} cannot be read back: {e}", target.display()))
    })?;
    let read_back = parsed
        .get("agents")
        .and_then(|a| a.get("capacity"))
        .and_then(|c| c.get("floor"))
        .and_then(toml::Value::as_integer);
    if read_back != Some(i64::from(floor)) {
        return Err(FloorError::refuse(
            1,
            format!(
                "readback did not show floor = {floor} in {}",
                target.display()
            ),
        ));
    }
    Ok(FloorReceipt {
        old,
        new: floor,
        target,
        unchanged: false,
        max_live: max_live(cwd),
    })
}

/// `agents.max_live` from the same ladder the gate's ceiling read walks, for
/// the receipt's clamp line.
fn max_live(cwd: &Path) -> Option<usize> {
    config_str(cwd, "agents", "max_live").and_then(|s| s.trim().parse::<usize>().ok())
}

/// The floor as the gate reads it: a safe walk, because `Index` panics on a
/// non-table item partway down the path (`agents = 4` above the key).
fn read_floor_int(doc: &DocumentMut) -> Option<i64> {
    doc.get("agents")?
        .as_table()?
        .get("capacity")?
        .as_table()?
        .get("floor")?
        .as_integer()
}

/// The gate's global rung: the config.toml SIBLING of
/// `$FNO_GLOBAL_SETTINGS_PATH` when set, else `$HOME/.fno/config.toml`.
/// Mirrored from `fno_agents::agents_config::global_config_path`, which this
/// crate cannot call (fno-agents depends on fno). Writing the pinned file
/// itself would land the floor where the gate never reads.
fn gate_global_config() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FNO_GLOBAL_SETTINGS_PATH").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p).with_file_name("config.toml"));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".fno/config.toml"))
}

/// Take `&mut Item` to a real table: existing tables pass through, missing
/// ones become empty tables, and anything else (a string, an inline table)
/// is a refusal, because editing it would destroy the value there.
fn ensure_table<'a>(
    item: &'a mut Item,
    place: &str,
    target: &Path,
) -> Result<&'a mut Table, FloorError> {
    match item {
        Item::Table(table) => Ok(table),
        Item::None => {
            *item = Item::Table(Table::new());
            Ok(item
                .as_table_mut()
                .expect("a Table was just stored in this Item"))
        }
        _ => Err(FloorError::refuse(
            2,
            format!(
                "{}: {place} exists as a non-table value; nothing written.",
                target.display()
            ),
        )),
    }
}

/// Write `body` over `target` through a same-directory temp file with the
/// given mode, fsynced before the rename, so a crash never leaves a half
/// config and the file's permissions survive the write.
fn write_atomic(target: &Path, body: &str, mode: fs::Permissions) -> Result<(), FloorError> {
    let parent = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".{name}.tmp-{}-{nanos}", std::process::id()));
    let write = || -> Result<(), FloorError> {
        let mut file = fs::File::create(&tmp).map_err(|e| {
            FloorError::refuse(1, format!("{} cannot be created: {e}", tmp.display()))
        })?;
        file.set_permissions(mode).map_err(|e| {
            FloorError::refuse(1, format!("{} cannot take its mode: {e}", tmp.display()))
        })?;
        file.write_all(body.as_bytes()).map_err(|e| {
            FloorError::refuse(1, format!("{} cannot be written: {e}", tmp.display()))
        })?;
        file.sync_all()
            .map_err(|e| FloorError::refuse(1, format!("{} cannot be synced: {e}", tmp.display())))
    };
    match write() {
        Ok(()) => {}
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    }
    fs::rename(&tmp, target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        FloorError::refuse(
            1,
            format!("{} cannot replace the target: {e}", tmp.display()),
        )
    })
}

/// The Python writer's config lock (`config.toml.lock` beside the target),
/// held across the read-modify-write so a concurrent `fno config set` and
/// this setter serialize. `LOCK_NB` with a 5-second poll, then a refusal.
struct ConfigLock {
    file: fs::File,
}

impl ConfigLock {
    fn acquire(target: &Path) -> Result<Self, FloorError> {
        let mut name = target.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        let lock_path = target.with_file_name(name);
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                FloorError::refuse(
                    1,
                    format!("config lock {} cannot open: {e}", lock_path.display()),
                )
            })?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self { file });
            }
            if Instant::now() >= deadline {
                return Err(FloorError::refuse(
                    1,
                    format!("config lock busy at {}", lock_path.display()),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.file), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peel_of(argv: &[&str]) -> Result<Peeled, String> {
        peel(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn peel_takes_the_floor_before_the_fence_and_refuses_bad_values() {
        let p = peel_of(&["agents", "spawn", "--floor", "8"]).unwrap();
        assert_eq!(p.floor, Some(8));
        assert_eq!(p.rest, vec!["agents", "spawn"]);
        assert!(!p.help);
        assert_eq!(
            peel_of(&["agents", "spawn", "--floor=8"]).unwrap().floor,
            Some(8)
        );
        let p = peel_of(&["agents", "spawn", "--floor", "8", "w1", "build", "it"]).unwrap();
        assert_eq!(p.rest, vec!["agents", "spawn", "w1", "build", "it"]);
        // Past the fence the flag is the harness's: untouched, no floor.
        let p = peel_of(&["agents", "spawn", "--", "--floor", "3"]).unwrap();
        assert_eq!(p.floor, None);
        assert_eq!(p.rest, vec!["agents", "spawn", "--", "--floor", "3"]);
        // Help is recognized with and without a floor, and stays in rest.
        let p = peel_of(&["agents", "spawn", "--help"]).unwrap();
        assert!(p.help && p.floor.is_none());
        let p = peel_of(&["agents", "spawn", "--floor", "8", "-h"]).unwrap();
        assert!(p.help && p.floor == Some(8));
        assert_eq!(p.rest, vec!["agents", "spawn", "-h"]);
        for (argv, frag) in [
            (vec!["agents", "spawn", "--floor", "0"], "1 or more"),
            (
                vec!["agents", "spawn", "--floor", "x"],
                "not a whole number",
            ),
            (
                vec!["agents", "spawn", "--floor", "--force"],
                "not a whole number",
            ),
            (vec!["agents", "spawn", "--floor"], "needs a whole number"),
            (
                vec!["agents", "spawn", "--floor", "2", "--floor", "3"],
                "given twice",
            ),
            (
                vec!["agents", "spawn", "--floor=2", "--floor=3"],
                "given twice",
            ),
        ] {
            let err = peel_of(&argv).expect_err("a refusal case passed");
            assert!(err.contains(frag), "{argv:?}: {err}");
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fno-spawn-floor-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn set_floor_keeps_comments_creates_the_table_and_refuses_a_shadow() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("write");
        let target = dir.join("config.toml");
        fs::write(&target, "# my cap\n[agents]\nmax_live = 24\n").unwrap();
        // A real user config sits at 0600; the setter preserves its mode.
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let receipt = set_floor_at(8, &target, &[], &dir).unwrap();
        assert!(!receipt.unchanged);
        let body = fs::read_to_string(&target).unwrap();
        assert!(body.contains("# my cap"), "{body}");
        assert!(body.contains("max_live = 24"), "{body}");
        assert!(body.contains("[agents.capacity]\nfloor = 8"), "{body}");
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a written file keeps 0600");
        // A target the setter creates lands at 0600 without a chmod.
        let fresh = dir.join("fresh.toml");
        set_floor_at(3, &fresh, &[], &dir).unwrap();
        let mode = fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a created file lands at 0600");
        assert!(
            receipt.to_string().contains("1 (default) -> 8"),
            "{receipt}"
        );
        // A second identical call writes nothing and says so.
        let before = fs::read_to_string(&target).unwrap();
        let receipt = set_floor_at(8, &target, &[], &dir).unwrap();
        assert!(receipt.unchanged);
        assert!(receipt.to_string().contains("(unchanged)"), "{receipt}");
        assert_eq!(fs::read_to_string(&target).unwrap(), before);
        // A shadow file the gate reads first refuses, and the target stays
        // byte-identical.
        let shadow_dir = temp_dir("shadow");
        let shadow = shadow_dir.join(".fno/config.toml");
        fs::create_dir_all(shadow.parent().unwrap()).unwrap();
        fs::write(&shadow, "[agents.capacity]\nfloor = 3\n").unwrap();
        let err = set_floor_at(8, &target, &[shadow.clone()], &dir).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.message.contains(&shadow.display().to_string()), "{err}");
        assert_eq!(fs::read_to_string(&target).unwrap(), before);
        // Invalid TOML refuses, writes nothing, leaves the file identical.
        let bad = dir.join("broken.toml");
        fs::write(&bad, "not toml ][\n").unwrap();
        let err = set_floor_at(8, &bad, &[], &dir).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.message.contains("not valid TOML"), "{err}");
        assert_eq!(fs::read_to_string(&bad).unwrap(), "not toml ][\n");
        // A non-table [agents] refuses the same way.
        let scalar = dir.join("scalar.toml");
        fs::write(&scalar, "agents = 4\n").unwrap();
        let err = set_floor_at(8, &scalar, &[], &dir).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.message.contains("non-table"), "{err}");
        assert_eq!(fs::read_to_string(&scalar).unwrap(), "agents = 4\n");
        // A target that cannot be read (invalid UTF-8 here) refuses instead
        // of being replaced as an empty config.
        let unreadable = dir.join("binary.toml");
        fs::write(&unreadable, [0xff_u8, 0xfe, 0x01]).unwrap();
        let err = set_floor_at(8, &unreadable, &[], &dir).unwrap_err();
        assert_eq!(err.code, 1);
        assert!(err.message.contains("cannot be read"), "{err}");
        assert_eq!(fs::read(&unreadable).unwrap(), [0xff_u8, 0xfe, 0x01]);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&shadow_dir);
    }

    #[test]
    fn floor_receipt_prints_the_clamp_line_only_above_the_ceiling() {
        let render = |max_live| FloorReceipt {
            old: Some(3),
            new: 8,
            target: PathBuf::from("/tmp/x/config.toml"),
            unchanged: false,
            max_live,
        };
        let under = render(Some(6)).to_string();
        assert!(
            under.contains("agents.max_live is 6, so the floor holds the cap at 6"),
            "{under}"
        );
        assert!(
            under.contains("Past it, a spawn admits only while CPU and RAM read room"),
            "{under}"
        );
        assert!(
            under.contains("To raise it: fno config set agents.max_live N"),
            "{under}"
        );
        assert!(!render(None).to_string().contains("max_live is"));
        assert!(!render(Some(8)).to_string().contains("cap holds at"));
    }

    #[test]
    fn front_door_flags_fit_the_spawn_flag_cap() {
        let owners = include_str!("../../../cli/src/fno/agents/spawn_flag_owners.py");
        let rows = owners
            .lines()
            .filter(|l| l.starts_with("    \"--") && l.contains("\": FlagOwner("))
            .count();
        let cap = owners
            .lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix("SPAWN_FLAG_CAP = ")
                    .and_then(|v| v.parse::<usize>().ok())
            })
            .expect("SPAWN_FLAG_CAP is in the file");
        assert!(
            rows + FRONT_DOOR_SPAWN_FLAGS.len() <= cap,
            "spawn carries {rows} Python flags and {} front-door flags (cap {cap}): \
             pay for a new flag by deleting one",
            FRONT_DOOR_SPAWN_FLAGS.len()
        );
        for flag in FRONT_DOOR_SPAWN_FLAGS {
            assert!(
                !owners.contains(&format!("    \"{flag}\":")),
                "{flag} is both a Python flag and a front-door flag"
            );
        }
        // The printed default is pinned to the gate's own spelling, so the
        // two crates cannot drift apart.
        assert!(
            include_str!("../../fno-agents/src/capacity.rs")
                .contains("pub const DEFAULT_FLOOR: u32 = 1;"),
            "the gate's DEFAULT_FLOOR moved; GATE_DEFAULT_FLOOR here must move with it"
        );
        // The setter's global target mirrors the gate's sibling rule; this
        // crate cannot call it (fno-agents depends on fno).
        assert!(
            include_str!("../../fno-agents/src/agents_config.rs")
                .contains("with_file_name(\"config.toml\")"),
            "the gate's global path rule moved; gate_global_config here must move with it"
        );
    }
}
