//! The worker-only `gh` proxy the front door installs before it hands a
//! command to the Python shell.
//!
//! The proxy is a two-line shim, `exec <fno-agents> gh "$@"`, in the state
//! root's `bin/github-cli` dir. The front door puts that dir first on PATH,
//! names it in `FNO_GH_PROXY_DIR`, and drops `FNO_REAL_GH`, so every `gh` the
//! Python shell or its children run goes through the quota broker. The shim
//! names fno-agents by absolute path: a reinstall that dropped the old
//! `fno-gh-proxy` helper once broke gh in every session.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const PROXY_DIR_ENV: &str = "FNO_GH_PROXY_DIR";
/// The proxy stamps this into the env of its own successor. It is good for
/// that one hop only: a descendant that inherits it has its first gh refused.
pub const PROXY_DEPTH_ENV: &str = "FNO_GH_PROXY_DEPTH";
const REAL_GH_ENV: &str = "FNO_REAL_GH";
const LEGACY_SHIM: &str = "#!/bin/sh\nexec fno-gh-proxy \"$@\"\n";

/// The state root's `bin/github-cli` dir: `$FNO_STATE_DIR` or `$HOME/.fno`,
/// never a settings load. The quota broker reads the same dir.
pub fn proxy_dir() -> Option<PathBuf> {
    let root = std::env::var_os("FNO_STATE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")))?;
    Some(crate::state_layout::place(&root, "bin").join("github-cli"))
}

/// The config-free proxy home for a state root that cannot be written.
pub fn fallback_proxy_dir() -> PathBuf {
    // SAFETY: getuid reads a per-process kernel value; it cannot fail or race.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir()
        .join(format!("fno-{uid}"))
        .join("github-cli")
}

/// `shlex.quote`: bare when every byte is shell-safe, else single-quoted.
pub fn shell_quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    if safe {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

/// The inverse of [`shell_quote`] for one word.
fn shell_unquote(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    c => out.push(c),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    c => out.push(c),
                }
            },
            c => out.push(c),
        }
    }
    Some(out)
}

pub fn wrapper(binary: &Path) -> String {
    format!(
        "#!/bin/sh\nexec {} gh \"$@\"\n",
        shell_quote(&binary.to_string_lossy())
    )
}

/// Any shim this installer wrote, current or from an older install.
pub fn is_wrapper(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    if text == LEGACY_SHIM {
        return true;
    }
    let Some(line) = text
        .strip_prefix("#!/bin/sh\n")
        .and_then(|rest| rest.lines().next())
    else {
        return false;
    };
    let Some(binary) = line
        .strip_prefix("exec ")
        .and_then(|rest| rest.strip_suffix(" gh \"$@\""))
        .and_then(shell_unquote)
    else {
        return false;
    };
    Path::new(&binary).file_name().and_then(|n| n.to_str()) == Some("fno-agents")
}

pub(crate) fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn which(name: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Temp roots a fixture or a fallback shim can live under, fixed per process
/// so the verdict cannot drift with the caller's env: a scrubbed-env driver
/// floors the temp dir to /tmp while the fixture sits under the OS temp root,
/// and one misread wrote the durable shim a sweep then deletes.
fn temp_bases() -> Vec<PathBuf> {
    let env = std::env::temp_dir();
    let mut bases = vec![resolve(&env), env];
    for base in [
        "/tmp",
        "/private/tmp",
        "/var/tmp",
        "/private/var/tmp",
        "/var/folders",
        "/private/var/folders",
    ] {
        let base = PathBuf::from(base);
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    bases
}

/// The path's own spellings, raw and resolved, strictly under a base: the
/// caller's tempdir need not be the binary's, and a sibling sharing the
/// prefix string is not under it.
pub(crate) fn in_temp(path: &Path) -> bool {
    let raw = path.to_path_buf();
    let resolved = resolve(path);
    temp_bases().iter().any(|base| {
        [raw.as_path(), resolved.as_path()]
            .into_iter()
            .any(|spelling| spelling != base && spelling.starts_with(base))
    })
}

/// The fno-agents a shim names. A cargo build dir is swept with its
/// worktree, so an installed copy on PATH wins over it.
fn shim_binary(path: &str) -> Option<PathBuf> {
    let binary = crate::digest_overlay::fno_agents_bin_auto()?;
    let in_cargo_target = binary
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|n| n == "target");
    if in_cargo_target {
        if let Some(installed) = which("fno-agents", path) {
            return Some(installed);
        }
    }
    Some(binary)
}

/// Write the shim into `root`, atomically, backing a foreign `gh` up to
/// `gh.pre-fno` once. Refuses a delegate that resolves back to the shim.
pub fn install_shim(root: &Path, binary: &Path, real_gh: &Path) -> Result<(), String> {
    let wrapper = wrapper(&std::path::absolute(binary).unwrap_or_else(|_| binary.to_path_buf()));
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    // Retired: the old shim exec'd this link by PATH name.
    let _ = std::fs::remove_file(root.join("fno-gh-proxy"));
    let proxy = root.join("gh");
    if resolve(real_gh) == resolve(&proxy) {
        return Err("resolved gh delegate points back to the Footnote proxy".into());
    }
    if std::fs::read_to_string(&proxy).ok().as_deref() == Some(wrapper.as_str()) {
        return Ok(());
    }
    if proxy.exists() && !is_wrapper(&proxy) {
        let backup = root.join("gh.pre-fno");
        if !backup.exists() {
            std::fs::copy(&proxy, &backup).map_err(|e| format!("{}: {e}", backup.display()))?;
        }
    }
    let mut tmp = tempfile::Builder::new()
        .prefix(".gh.")
        .tempfile_in(root)
        .map_err(|e| format!("{}: {e}", root.display()))?;
    tmp.write_all(wrapper.as_bytes())
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| format!("{}: {e}", proxy.display()))?;
    let mut mode = tmp
        .as_file()
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions();
    mode.set_mode(mode.mode() | 0o111);
    std::fs::set_permissions(tmp.path(), mode).map_err(|e| e.to_string())?;
    tmp.persist(&proxy).map_err(|e| e.error.to_string())?;
    Ok(())
}

/// Install the shim in `root` (the state-root dir when `None`). `Ok(None)`
/// is the degrade: no fno-agents to exec, or a temp-dir binary a durable
/// shim must not pin, so gh stays unwrapped and the quota policy stands down.
fn ensure_proxy(
    root: Option<&Path>,
    real_gh: &Path,
    path: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(binary) = shim_binary(path) else {
        return Ok(None);
    };
    if !is_executable(&binary) {
        // shim_binary checked runnable, but a sweep can delete the file
        // between resolve and install, and a shim that execs a missing
        // binary breaks every gh call on the machine.
        return Ok(None);
    }
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => proxy_dir().ok_or("no HOME and no FNO_STATE_DIR")?,
    };
    if in_temp(&binary) && !in_temp(&root) {
        return Ok(None);
    }
    install_shim(&root, &binary, real_gh)?;
    Ok(Some(root))
}

/// The env edits the front door applies before it execs the Python shell:
/// `Some` sets a key, `None` removes it. An inherited `FNO_GH_PROXY_DIR` is
/// never honored, so a parent's temp pin cannot win PATH over the durable home.
pub fn front_door_env() -> Vec<(&'static str, Option<OsString>)> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut edits: Vec<(&'static str, Option<OsString>)> =
        vec![(PROXY_DEPTH_ENV, None), (PROXY_DIR_ENV, None)];
    let inherited = std::env::var_os(REAL_GH_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let Some(delegate) = inherited.or_else(|| which("gh", &path)) else {
        return edits;
    };
    let installed = if is_wrapper(&delegate) {
        let parent = resolve(delegate.parent().unwrap_or(Path::new("/")));
        let durable = if parent == resolve(&fallback_proxy_dir()) {
            // The temp shim is reapable: move the lineage to the durable home.
            ensure_proxy(None, &delegate, &path).ok().flatten()
        } else {
            None
        };
        if durable.is_none() {
            // Refresh the inherited shim in place; on failure it stays as is.
            let outside = std::env::join_paths(
                std::env::split_paths(&path)
                    .filter(|p| !p.as_os_str().is_empty() && resolve(p) != parent),
            )
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
            if let Some(real) = which("gh", &outside) {
                let _ = ensure_proxy(Some(&parent), &real, &outside);
            }
            edits.push((PROXY_DIR_ENV, Some(parent.into_os_string())));
            edits.push((REAL_GH_ENV, None));
            return edits;
        }
        durable
    } else {
        ensure_proxy(None, &delegate, &path)
            .or_else(|_| ensure_proxy(Some(&fallback_proxy_dir()), &delegate, &path))
            .ok()
            .flatten()
    };
    let Some(dir) = installed else {
        return edits;
    };
    let mut prepended = dir.clone().into_os_string();
    if !path.is_empty() {
        prepended.push(":");
        prepended.push(&path);
    }
    edits.push(("PATH", Some(prepended)));
    edits.push((PROXY_DIR_ENV, Some(resolve(&dir).into_os_string())));
    edits.push((REAL_GH_ENV, None));
    edits
}

/// Apply [`front_door_env`] to the command the front door execs.
pub fn protect(command: &mut std::process::Command) {
    for (key, value) in front_door_env() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
}
